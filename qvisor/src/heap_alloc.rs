use libc;
use core::alloc::{GlobalAlloc, Layout};
use core::sync::atomic::AtomicBool;
use core::sync::atomic::Ordering;


use super::qlib::mem::list_allocator::*;
use super::qlib::linux_def::MemoryDef;

pub const KERNEL_HEAP_ORD : usize = 33; // 16GB
const HEAP_OFFSET: u64 = 1 * MemoryDef::ONE_GB;

#[derive(Debug)]
pub struct HostAllocator {
    pub listHeapAddr : u64,
    pub initialized: AtomicBool
}

impl HostAllocator {
    pub const fn New() -> Self {
        return Self {
            listHeapAddr: MemoryDef::PHY_LOWER_ADDR + HEAP_OFFSET,
            initialized: AtomicBool::new(false)
        }
    }

    pub fn Allocator(&self) -> &mut ListAllocator {
        return unsafe {
            &mut *(self.listHeapAddr as * mut ListAllocator)
        }
    }

    pub fn Init(&self) {
        let heapSize = 1 << KERNEL_HEAP_ORD as usize;
        let addr = unsafe {
            libc::mmap(self.listHeapAddr as _,
                       heapSize,
                       libc::PROT_READ | libc::PROT_WRITE,
                       libc::MAP_PRIVATE | libc::MAP_ANON | libc::MAP_FIXED,
                       -1,
                       0) as u64
        };

        if addr == libc::MAP_FAILED as u64 {
            panic!("mmap: failed to get mapped memory area for heap");
        }

        assert!(self.listHeapAddr == addr, "listHeapAddr is {:x}, addr is {:x}", self.listHeapAddr, addr);

        *self.Allocator() = ListAllocator::Empty();

        // reserve first 4KB gor the listAllocator
        self.Allocator().Add(addr as usize + 0x2000, heapSize - 0x2000);
        self.initialized.store(true, Ordering::Relaxed);
    }
}

unsafe impl GlobalAlloc for HostAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let initialized = self.initialized.load(Ordering::Relaxed);
        if !initialized {
            self.Init();
        }

        return self.Allocator().alloc(layout)
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        self.Allocator().dealloc(ptr, layout);
    }
}

impl OOMHandler for ListAllocator {
    fn handleError(&self, _a:u64, _b:u64) {
        panic!("qvisor OOM: Heap allocator fails to allocate memory block");
    }
}

impl ListAllocator {
    pub fn initialize(&self) {
        let listHeapAddr = MemoryDef::PHY_LOWER_ADDR + HEAP_OFFSET;
        let heapSize = 1 << KERNEL_HEAP_ORD as usize;
        let address: usize;
        unsafe {
            address = libc::mmap(listHeapAddr as _, heapSize, libc::PROT_READ | libc::PROT_WRITE,
                                 libc::MAP_PRIVATE | libc::MAP_ANON, -1, 0) as usize;
            if address == libc::MAP_FAILED as usize {
                panic!("mmap: failed to get mapped memory area for heap");
            }
            self.heap.lock().init(address + 0x1000 as usize, heapSize - 0x1000);
        }
        self.initialized.store(true, Ordering::Relaxed);
    }

    pub fn Check(&self) {
    }
}

impl VcpuAllocator {
    pub fn handleError(&self, _size:u64, _alignment:u64) {

    }
}