#![allow(clippy::upper_case_acronyms)]

use core::arch::asm;
use core::marker::PhantomData;

use x86_64::structures::paging::RecursivePageTable;

use crate::arch::x86_64::physicalmem;

/// Pointer to the root page table (PML4)
const PML4_ADDRESS: *mut PageTable<PML4> = 0xFFFF_FFFF_FFFF_F000 as *mut PageTable<PML4>;

/// Number of Offset bits of a virtual address for a 4 KiB page, which are shifted away to get its Page Frame Number (PFN).
const PAGE_BITS: usize = 12;

/// Number of bits of the index in each table (PML4, PDPT, PDT, PGT).
const PAGE_MAP_BITS: usize = 9;

/// A mask where PAGE_MAP_BITS are set to calculate a table index.
const PAGE_MAP_MASK: usize = 0x1FF;

bitflags::bitflags! {
	/// Possible flags for an entry in either table (PML4, PDPT, PDT, PGT)
	///
	/// See Intel Vol. 3A, Tables 4-14 through 4-19
	pub struct PageTableEntryFlags: usize {
		/// Set if this entry is valid and points to a page or table.
		const PRESENT = 1 << 0;

		/// Set if memory referenced by this entry shall be writable.
		const WRITABLE = 1 << 1;

		/// Set if memory referenced by this entry shall be accessible from user-mode (Ring 3).
		const USER_ACCESSIBLE = 1 << 2;

		/// Set if Write-Through caching shall be enabled for memory referenced by this entry.
		/// Otherwise, Write-Back caching is used.
		const WRITE_THROUGH = 1 << 3;

		/// Set if caching shall be disabled for memory referenced by this entry.
		const CACHE_DISABLE = 1 << 4;

		/// Set if software has accessed this entry (for memory access or address translation).
		const ACCESSED = 1 << 5;

		/// Only for page entries: Set if software has written to the memory referenced by this entry.
		const DIRTY = 1 << 6;

		/// Only for page entries in PDPT or PDT: Set if this entry references a 1 GiB (PDPT) or 2 MiB (PDT) page.
		const HUGE_PAGE = 1 << 7;

		/// Only for page entries: Set if this address translation is global for all tasks and does not need to
		/// be flushed from the TLB when CR3 is reset.
		const GLOBAL = 1 << 8;

		/// Set if code execution shall be disabled for memory referenced by this entry.
		const EXECUTE_DISABLE = 1 << 63;
	}
}

impl PageTableEntryFlags {
	/// An empty set of flags for unused/zeroed table entries.
	/// Needed as long as empty() is no const function.
	const BLANK: PageTableEntryFlags = PageTableEntryFlags { bits: 0 };
}

/// An entry in either table (PML4, PDPT, PDT, PGT)
#[derive(Clone, Copy)]
pub struct PageTableEntry {
	/// Physical memory address this entry refers, combined with flags from PageTableEntryFlags.
	physical_address_and_flags: usize,
}

impl PageTableEntry {
	/// Returns whether this entry is valid (present).
	fn is_present(&self) -> bool {
		(self.physical_address_and_flags & PageTableEntryFlags::PRESENT.bits()) != 0
	}

	/// Mark this as a valid (present) entry and set address translation and flags.
	///
	/// # Arguments
	///
	/// * `physical_address` - The physical memory address this entry shall translate to
	/// * `flags` - Flags from PageTableEntryFlags (note that the PRESENT and ACCESSED flags are set automatically)
	fn set(&mut self, physical_address: usize, flags: PageTableEntryFlags) {
		if flags.contains(PageTableEntryFlags::HUGE_PAGE) {
			// HUGE_PAGE may indicate a 2 MiB or 1 GiB page.
			// We don't know this here, so we can only verify that at least the offset bits for a 2 MiB page are zero.
			assert_eq!(
				physical_address % LargePageSize::SIZE as usize,
				0,
				"Physical address is not on a 2 MiB page boundary (physical_address = {:#x})",
				physical_address
			);
		} else {
			// Verify that the offset bits for a 4 KiB page are zero.
			assert_eq!(
				physical_address % BasePageSize::SIZE as usize,
				0,
				"Physical address is not on a 4 KiB page boundary (physical_address = {:#x})",
				physical_address
			);
		}

		self.physical_address_and_flags = physical_address
			| (PageTableEntryFlags::PRESENT | PageTableEntryFlags::ACCESSED | flags).bits();
	}
}

/// A generic interface to support all possible page sizes.
///
/// This is defined as a subtrait of Copy to enable #[derive(Clone, Copy)] for Page.
/// Currently, deriving implementations for these traits only works if all dependent types implement it as well.
pub trait PageSize: Copy {
	/// The page size in bytes.
	const SIZE: u64;

	/// The page table level at which a page of this size is mapped (from 0 for PGT through 3 for PML4).
	/// Implemented as a numeric value to enable numeric comparisons.
	const MAP_LEVEL: usize;

	/// Any extra flag that needs to be set to map a page of this size.
	/// For example: PageTableEntryFlags::HUGE_PAGE
	const MAP_EXTRA_FLAG: PageTableEntryFlags;
}

/// A 4 KiB page mapped in the PGT.
#[derive(Clone, Copy)]
pub enum BasePageSize {}
impl PageSize for BasePageSize {
	const SIZE: u64 = 4096;
	const MAP_LEVEL: usize = 0;
	const MAP_EXTRA_FLAG: PageTableEntryFlags = PageTableEntryFlags::BLANK;
}

/// A 2 MiB page mapped in the PDT.
#[derive(Clone, Copy)]
pub enum LargePageSize {}
impl PageSize for LargePageSize {
	const SIZE: u64 = 2 * 1024 * 1024;
	const MAP_LEVEL: usize = 1;
	const MAP_EXTRA_FLAG: PageTableEntryFlags = PageTableEntryFlags::HUGE_PAGE;
}

/// A memory page of the size given by S.
#[derive(Clone, Copy)]
struct Page<S: PageSize> {
	/// Virtual memory address of this page.
	/// This is rounded to a page size boundary on creation.
	virtual_address: usize,

	/// Required by Rust to support the S parameter.
	size: PhantomData<S>,
}

impl<S: PageSize> Page<S> {
	/// Flushes this page from the TLB of this CPU.
	fn flush_from_tlb(&self) {
		unsafe {
			asm!("invlpg [{}]", in(reg) self.virtual_address, options(nostack, preserves_flags));
		}
	}

	/// Returns whether the given virtual address is a valid one in the x86-64 memory model.
	///
	/// Current x86-64 supports only 48-bit for virtual memory addresses.
	/// This is enforced by requiring bits 63 through 48 to replicate bit 47 (cf. Intel Vol. 1, 3.3.7.1).
	/// As a consequence, the address space is divided into the two valid regions 0x8000_0000_0000
	/// and 0xFFFF_8000_0000_0000.
	///
	/// Although we could make this check depend on the actual linear address width from the CPU,
	/// any extension above 48-bit would require a new page table level, which we don't implement.
	fn is_valid_address(virtual_address: usize) -> bool {
		!(0x8000_0000_0000..0xFFFF_8000_0000_0000).contains(&virtual_address)
	}

	/// Returns a Page including the given virtual address.
	/// That means, the address is rounded down to a page size boundary.
	fn including_address(virtual_address: usize) -> Self {
		assert!(Self::is_valid_address(virtual_address));

		Self {
			virtual_address: align_down!(virtual_address, S::SIZE as usize),
			size: PhantomData,
		}
	}

	/// Returns a PageIter to iterate from the given first Page to the given last Page (inclusive).
	fn range(first: Self, last: Self) -> PageIter<S> {
		assert!(first.virtual_address <= last.virtual_address);
		PageIter {
			current: first,
			last,
		}
	}

	/// Returns the index of this page in the table given by L.
	fn table_index<L: PageTableLevel>(&self) -> usize {
		assert!(L::LEVEL >= S::MAP_LEVEL);
		self.virtual_address >> PAGE_BITS >> (L::LEVEL * PAGE_MAP_BITS) & PAGE_MAP_MASK
	}
}

/// An iterator to walk through a range of pages of size S.
struct PageIter<S: PageSize> {
	current: Page<S>,
	last: Page<S>,
}

impl<S: PageSize> Iterator for PageIter<S> {
	type Item = Page<S>;

	fn next(&mut self) -> Option<Page<S>> {
		if self.current.virtual_address <= self.last.virtual_address {
			let p = self.current;
			self.current.virtual_address += S::SIZE as usize;
			Some(p)
		} else {
			None
		}
	}
}

/// An interface to allow for a generic implementation of struct PageTable for all 4 page tables.
/// Must be implemented by all page tables.
trait PageTableLevel {
	/// Numeric page table level (from 0 for PGT through 3 for PML4) to enable numeric comparisons.
	const LEVEL: usize;
}

/// An interface for page tables with sub page tables (all except PGT).
/// Having both PageTableLevel and PageTableLevelWithSubtables leverages Rust's typing system to provide
/// a next_table_for_page method only for those that have sub page tables.
///
/// Kudos to Philipp Oppermann for the trick!
trait PageTableLevelWithSubtables: PageTableLevel {
	type SubtableLevel;
}

/// The Page Map Level 4 (PML4) table, with numeric level 3 and PDPT subtables.
enum PML4 {}
impl PageTableLevel for PML4 {
	const LEVEL: usize = 3;
}

impl PageTableLevelWithSubtables for PML4 {
	type SubtableLevel = PDPT;
}

/// A Page Directory Pointer Table (PDPT), with numeric level 2 and PDT subtables.
enum PDPT {}
impl PageTableLevel for PDPT {
	const LEVEL: usize = 2;
}

impl PageTableLevelWithSubtables for PDPT {
	type SubtableLevel = PDT;
}

/// A Page Directory Table (PDT), with numeric level 1 and PGT subtables.
enum PDT {}
impl PageTableLevel for PDT {
	const LEVEL: usize = 1;
}

impl PageTableLevelWithSubtables for PDT {
	type SubtableLevel = PGT;
}

/// A Page Table (PGT), with numeric level 0 and no subtables.
enum PGT {}
impl PageTableLevel for PGT {
	const LEVEL: usize = 0;
}

/// Representation of any page table (PML4, PDPT, PDT, PGT) in memory.
/// Parameter L supplies information for Rust's typing system to distinguish between the different tables.
struct PageTable<L> {
	/// Each page table has 512 entries (can be calculated using PAGE_MAP_BITS).
	entries: [PageTableEntry; 1 << PAGE_MAP_BITS],

	/// Required by Rust to support the L parameter.
	level: PhantomData<L>,
}

trait LocallyMappable {
	fn map_page_in_this_table<S: PageSize>(
		&mut self,
		page: Page<S>,
		physical_address: usize,
		flags: PageTableEntryFlags,
	) -> bool;
}

trait Mappable: LocallyMappable {
	fn map_page<S: PageSize>(
		&mut self,
		page: Page<S>,
		physical_address: usize,
		flags: PageTableEntryFlags,
	) -> bool;
}

impl<L: PageTableLevel> LocallyMappable for PageTable<L> {
	/// Maps a single page in this table to the given physical address.
	/// Returns whether an existing entry was updated. You can use this return value to flush TLBs.
	///
	/// Must only be called if a page of this size is mapped at this page table level!
	fn map_page_in_this_table<S: PageSize>(
		&mut self,
		page: Page<S>,
		physical_address: usize,
		flags: PageTableEntryFlags,
	) -> bool {
		assert_eq!(L::LEVEL, S::MAP_LEVEL);
		let index = page.table_index::<L>();
		let flush = self.entries[index].is_present();

		self.entries[index].set(
			physical_address,
			PageTableEntryFlags::DIRTY | S::MAP_EXTRA_FLAG | flags,
		);

		if flush {
			page.flush_from_tlb();
		}

		flush
	}
}

impl Mappable for PageTable<PGT> {
	/// Maps a single page to the given physical address.
	/// Returns whether an existing entry was updated. You can use this return value to flush TLBs.
	fn map_page<S: PageSize>(
		&mut self,
		page: Page<S>,
		physical_address: usize,
		flags: PageTableEntryFlags,
	) -> bool {
		self.map_page_in_this_table::<S>(page, physical_address, flags)
	}
}

impl<L: PageTableLevelWithSubtables> Mappable for PageTable<L>
where
	L::SubtableLevel: PageTableLevel,
	PageTable<L::SubtableLevel>: Mappable,
{
	/// Maps a single page to the given physical address.
	/// Returns whether an existing entry was updated. You can use this return value to flush TLBs.
	///
	/// This is the implementation for all tables with subtables (PML4, PDPT, PDT).
	fn map_page<S: PageSize>(
		&mut self,
		page: Page<S>,
		physical_address: usize,
		flags: PageTableEntryFlags,
	) -> bool {
		assert!(L::LEVEL >= S::MAP_LEVEL);

		if L::LEVEL > S::MAP_LEVEL {
			let index = page.table_index::<L>();

			// Does the table exist yet?
			if !self.entries[index].is_present() {
				// Allocate a single 4 KiB page for the new entry and mark it as a valid, writable subtable.
				let physical_address = physicalmem::allocate(BasePageSize::SIZE as usize);
				self.entries[index].set(physical_address, PageTableEntryFlags::WRITABLE);

				// Mark all entries as unused in the newly created table.
				let subtable = self.subtable::<S>(page);
				for entry in subtable.entries.iter_mut() {
					entry.physical_address_and_flags = 0;
				}
			}

			let subtable = self.subtable::<S>(page);
			subtable.map_page::<S>(page, physical_address, flags)
		} else {
			// Calling the default implementation from a specialized one is not supported (yet),
			// so we have to resort to an extra function.
			self.map_page_in_this_table::<S>(page, physical_address, flags)
		}
	}
}

impl<L: PageTableLevelWithSubtables> PageTable<L>
where
	L::SubtableLevel: PageTableLevel,
	PageTable<L::SubtableLevel>: Mappable,
{
	/// Returns the next subtable for the given page in the page table hierarchy.
	///
	/// Must only be called if a page of this size is mapped in a subtable!
	fn subtable<S: PageSize>(&mut self, page: Page<S>) -> &mut PageTable<L::SubtableLevel> {
		assert!(L::LEVEL > S::MAP_LEVEL);

		// Calculate the address of the subtable.
		let index = page.table_index::<L>();
		let table_address = self as *const PageTable<L> as usize;
		let subtable_address = (table_address << PAGE_MAP_BITS) | (index << PAGE_BITS);
		unsafe { &mut *(subtable_address as *mut PageTable<L::SubtableLevel>) }
	}

	/// Maps a continuous range of pages.
	///
	/// # Arguments
	///
	/// * `range` - The range of pages of size S
	/// * `physical_address` - First physical address to map these pages to
	/// * `flags` - Flags from PageTableEntryFlags to set for the page table entry (e.g. WRITABLE or EXECUTE_DISABLE).
	///             The PRESENT, ACCESSED, and DIRTY flags are already set automatically.
	fn map_pages<S: PageSize>(
		&mut self,
		range: PageIter<S>,
		physical_address: usize,
		flags: PageTableEntryFlags,
	) {
		let mut current_physical_address = physical_address;

		for page in range {
			self.map_page::<S>(page, current_physical_address, flags);
			current_physical_address += S::SIZE as usize;
		}
	}
}

#[inline]
fn get_page_range<S: PageSize>(virtual_address: usize, count: usize) -> PageIter<S> {
	let first_page = Page::<S>::including_address(virtual_address);
	let last_page = Page::<S>::including_address(virtual_address + (count - 1) * S::SIZE as usize);
	Page::range(first_page, last_page)
}

pub fn map<S: PageSize>(
	virtual_address: usize,
	physical_address: usize,
	count: usize,
	flags: PageTableEntryFlags,
) {
	println!("virtual_address = {virtual_address:#x}");
	println!("physical_address = {physical_address:#x}");
	println!("count = {count}");
	println!("flags = {flags:?}");
	unsafe {
		print_page_tables(4);
	}
	let range = get_page_range::<S>(virtual_address, count);
	let root_pagetable = unsafe { &mut *PML4_ADDRESS };
	root_pagetable.map_pages(range, physical_address, flags);
	unsafe {
		disect(x86_64::VirtAddr::new(virtual_address as u64));
	}
}

unsafe fn recursive_page_table() -> RecursivePageTable<'static> {
	let level_4_table_addr = 0xFFFF_FFFF_FFFF_F000_usize;
	let level_4_table_ptr = level_4_table_addr as *mut _;
	unsafe {
		let level_4_table = &mut *(level_4_table_ptr);
		RecursivePageTable::new(level_4_table).unwrap()
	}
}

#[allow(dead_code)]
unsafe fn disect(virt_addr: x86_64::VirtAddr) {
	use x86_64::structures::paging::mapper::{MappedFrame, TranslateResult};
	use x86_64::structures::paging::{Size1GiB, Size4KiB, Translate, Page, Size2MiB};

	let recursive_page_table = unsafe { recursive_page_table() };

	match recursive_page_table.translate(virt_addr) {
		TranslateResult::Mapped {
			frame,
			offset,
			flags,
		} => {
			let phys_addr = frame.start_address() + offset;
			println!("virt_addr: {virt_addr:p}, phys_addr: {phys_addr:p}, flags: {flags:?}");
			match frame {
				MappedFrame::Size4KiB(_) => {
					let page = Page::<Size4KiB>::containing_address(virt_addr);
					println!(
						"p4: {}, p3: {}, p2: {}, p1: {}",
						u16::from(page.p4_index()),
						u16::from(page.p3_index()),
						u16::from(page.p2_index()),
						u16::from(page.p1_index())
					);
				}
				MappedFrame::Size2MiB(_) => {
					let page = Page::<Size2MiB>::containing_address(virt_addr);
					println!(
						"p4: {}, p3: {}, p2: {}",
						u16::from(page.p4_index()),
						u16::from(page.p3_index()),
						u16::from(page.p2_index()),
					);
				}
				MappedFrame::Size1GiB(_) => {
					let page = Page::<Size1GiB>::containing_address(virt_addr);
					println!(
						"p4: {}, p3: {}",
						u16::from(page.p4_index()),
						u16::from(page.p3_index()),
					);
				}
			}
		}
		TranslateResult::NotMapped => println!("{virt_addr:p} not mapped"),
		TranslateResult::InvalidFrameAddress(_) => todo!(),
	}
}

#[allow(dead_code)]
unsafe fn print_page_tables(levels: usize) {
	use x86_64::structures::paging::PageTableFlags as PageTableEntryFlags;

	assert!((1..=4).contains(&levels));

	fn print(table: &x86_64::structures::paging::PageTable, level: usize, min_level: usize) {
		for (i, entry) in table.iter().filter(|entry| !entry.is_unused()).enumerate() {
			let indent = &"        "[0..2 * (4 - level)];
			println!("{indent}L{level} Entry {i}: {entry:?}",);

			if level > min_level && !entry.flags().contains(PageTableEntryFlags::HUGE_PAGE) {
				let phys = entry.frame().unwrap().start_address();
				let virt = x86_64::VirtAddr::new(phys.as_u64());
				let entry_table = unsafe { &*virt.as_mut_ptr() };

				print(entry_table, level - 1, min_level);
			}
		}
	}

	let mut recursive_page_table = unsafe { recursive_page_table() };
	print(recursive_page_table.level_4_table(), 4, 5 - levels);
}
