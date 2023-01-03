use x86_64::structures::paging::{FrameAllocator, PhysFrame};

use crate::arch::paging::{BasePageSize, PageSize};

static mut CURRENT_ADDRESS: usize = 0;

pub fn init(address: usize) {
	unsafe {
		CURRENT_ADDRESS = address;
	}
}

pub fn allocate(size: usize) -> usize {
	assert!(size > 0);
	assert_eq!(
		size % BasePageSize::SIZE as usize,
		0,
		"Size {:#x} is a multiple of {:#x}",
		size,
		BasePageSize::SIZE as usize
	);

	unsafe {
		assert!(CURRENT_ADDRESS > 0, "Trying to allocate physical memory before the Physical Memory Manager has been initialized");
		let address = CURRENT_ADDRESS;
		CURRENT_ADDRESS += size;
		println!("phys_allocate({size}) = {address:#x}");
		address
	}
}

pub struct FrameAlloc;

unsafe impl<S: x86_64::structures::paging::PageSize> FrameAllocator<S> for FrameAlloc {
	fn allocate_frame(&mut self) -> Option<PhysFrame<S>> {
		let addr = allocate(S::SIZE as usize) as u64;
		Some(PhysFrame::from_start_address(x86_64::PhysAddr::new(addr)).unwrap())
	}
}
