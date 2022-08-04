use core::fmt::Debug;

use x86_64::{
	registers::control::Cr3,
	structures::paging::{
		Mapper, OffsetPageTable, Page, PageSize, PageTable, PhysFrame, Size2MiB, Size4KiB,
	},
	PhysAddr, VirtAddr,
};

static mut MAPPER: Option<OffsetPageTable<'static>> = None;

fn new() -> OffsetPageTable<'static> {
	let (level_4_table_addr, _cr3_flags) = Cr3::read();

	let level_4_table_ptr = level_4_table_addr.start_address().as_u64() as *mut PageTable;
	let level_4_table = unsafe { &mut *level_4_table_ptr };

	unsafe { OffsetPageTable::new(level_4_table, VirtAddr::new(0)) }
}

pub unsafe fn mapper() -> &'static mut OffsetPageTable<'static> {
	MAPPER.get_or_insert_with(new)
}

pub type BasePageSize = Size4KiB;

pub type LargePageSize = Size2MiB;

pub trait PageSizeCompat {
	/// The page size in bytes.
	const SIZE_COMPAT: usize;
}

impl PageSizeCompat for BasePageSize {
	const SIZE_COMPAT: usize = <Self as PageSize>::SIZE as usize;
}

impl PageSizeCompat for LargePageSize {
	const SIZE_COMPAT: usize = <Self as PageSize>::SIZE as usize;
}

pub type PageTableEntryFlags = x86_64::structures::paging::PageTableFlags;

pub fn map<S: PageSize + Debug>(
	virtual_address: usize,
	physical_address: usize,
	count: usize,
	flags: PageTableEntryFlags,
) where
	OffsetPageTable<'static>: Mapper<S>,
{
	for i in 0..count as u64 {
		let page = Page::<S>::containing_address(VirtAddr::new(virtual_address as u64)) + i;
		let frame = PhysFrame::<S>::containing_address(PhysAddr::new(physical_address as u64)) + i;
		unsafe {
			mapper()
				.map_to(page, frame, flags, &mut super::physicalmem::FrameAlloc)
				.unwrap()
				.flush();
		}
	}
}
