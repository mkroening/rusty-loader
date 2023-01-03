use core::fmt::Debug;

pub use x86_64::structures::paging::PageTableFlags as PageTableEntryFlags;
use x86_64::structures::paging::{Mapper, Page, PhysFrame, RecursivePageTable, FrameDeallocator, mapper::CleanUp};
pub use x86_64::structures::paging::{
	PageSize, Size1GiB as HugePageSize, Size2MiB as LargePageSize, Size4KiB as BasePageSize,
};

/// Maps a continuous range of pages.
///
/// # Arguments
///
/// * `physical_address` - First physical address to map these pages to
/// * `flags` - Flags from PageTableEntryFlags to set for the page table entry (e.g. WRITABLE or NO_EXECUTE).
///             The PRESENT flags is set automatically.
pub fn map<S>(
	virtual_address: usize,
	physical_address: usize,
	count: usize,
	flags: PageTableEntryFlags,
) where
	S: PageSize + Debug,
	RecursivePageTable<'static>: Mapper<S>,
{
	println!("virtual_address = {virtual_address:#x}");
	println!("physical_address = {physical_address:#x}");
	println!("count = {count}");
	println!("flags = {flags:?}");
	unsafe {
		print_page_tables(4);
	}
	
	let pages = {
		let start = Page::<S>::containing_address(x86_64::VirtAddr::new(virtual_address as u64));
		let end = start + count as u64;
		Page::range(start, end)
	};

	let frames = {
		let start =
			PhysFrame::<S>::containing_address(x86_64::PhysAddr::new(physical_address as u64));
		let end = start + count as u64;
		PhysFrame::range(start, end)
	};

	let flags = flags | PageTableEntryFlags::PRESENT | PageTableEntryFlags::ACCESSED | PageTableEntryFlags::DIRTY;

	let mut table = unsafe { recursive_page_table() };

	for (page, frame) in pages.zip(frames) {
		unsafe {
			// TODO: Require explicit unmaps
			if let Ok((_frame, flush)) = table.unmap(page) {
				flush.flush();
			}
			table
				.map_to(page, frame, flags, &mut super::physicalmem::FrameAlloc)
				.unwrap()
				.flush();
		}
	}
	
	unsafe {
		disect(x86_64::VirtAddr::new(virtual_address as u64));
	}
}

pub fn unmap(page: Page) {
	let mut table = unsafe { recursive_page_table() };

	let (_frame, flush) = table.unmap(page).unwrap();
	flush.flush();
}

pub fn clean_up() {
	let mut table = unsafe { recursive_page_table() };

	struct Foo;

	impl<S: PageSize> FrameDeallocator<S> for Foo {
		unsafe fn deallocate_frame(&mut self, frame: PhysFrame<S>) {

		}
	}

	unsafe { table.clean_up(&mut Foo) }
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
	use x86_64::structures::paging::{Page, Size1GiB, Size2MiB, Size4KiB, Translate};

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
