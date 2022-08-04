use core::sync::atomic::{Ordering, AtomicU64};

use x86_64::{structures::paging::{PageSize, FrameAllocator, PhysFrame}, PhysAddr};

use super::paging::{BasePageSize, PageSizeCompat};

static CURRENT_ADDR: AtomicU64 = AtomicU64::new(0);

pub fn init(address: usize) {
	CURRENT_ADDR.store(address as u64, Ordering::Relaxed);
}

pub fn allocate(size: usize) -> usize {
	assert!(size > 0);
	assert_eq!(
		size % BasePageSize::SIZE_COMPAT,
		0,
		"Size {:#x} is a multiple of {:#x}",
		size,
		BasePageSize::SIZE_COMPAT
	);
	assert_ne!(0, CURRENT_ADDR.load(Ordering::Relaxed));

	CURRENT_ADDR.fetch_add(size as u64, Ordering::Relaxed) as usize
}

pub struct FrameAlloc;

unsafe impl<S: PageSize> FrameAllocator<S> for FrameAlloc {
    fn allocate_frame(&mut self) -> Option<PhysFrame<S>> {
		let current_addr = PhysAddr::new(CURRENT_ADDR.load(Ordering::Relaxed));
		assert!(!current_addr.is_null());
		let frame = PhysFrame::containing_address(current_addr);
		let frame = if frame.start_address() == current_addr {
			frame
		} else {
			frame + 1
		};
		CURRENT_ADDR.store(frame.start_address().as_u64() + frame.size(), Ordering::Relaxed);
		Some(frame)
    }
}
