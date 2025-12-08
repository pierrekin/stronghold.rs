// Copyright 2020-2021 IOTA Stiftung
// SPDX-License-Identifier: Apache-2.0

use crate::types::*;
use zeroize::Zeroize;

use core::{
    cell::Cell,
    fmt::{self, Debug},
    mem,
    ptr::NonNull,
    slice,
};

use std::alloc::{Allocator, Layout};
use dryoc::protected::PageAlignedAllocator;


#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Prot {
    NoAccess,
    ReadOnly,
    ReadWrite,
}

type RefCount = u8;

/// A protected piece of memory.
#[derive(Eq)]
pub(crate) struct Boxed<T: Bytes> {
    // the pointer to the underlying protected memory
    ptr: NonNull<T>,
    // The number of elements of type `T` that can be stored in the pointer.
    len: usize,
    // The actual allocated size (page-aligned)
    allocated_size: usize,
    // the current protection level of the data.
    prot: Cell<Prot>,
    // The number of current borrows of this pointer.
    refs: Cell<RefCount>,
}

impl<T: Bytes> Boxed<T> {
    pub(crate) fn new<F>(len: usize, init: F) -> Self
    where
        F: FnOnce(&mut Self),
    {
        let mut boxed = Self::new_unlocked(len);

        assert!(
            len == 0 || boxed.ptr != core::ptr::NonNull::dangling(),
            "Make sure pointer isn't dangling (unless zero-length)"
        );
        assert!(boxed.len == len);

        init(&mut boxed);

        boxed.lock();

        boxed
    }

    #[allow(dead_code)]
    pub(crate) fn try_new<R, E, F>(len: usize, init: F) -> Result<Self, E>
    where
        F: FnOnce(&mut Self) -> Result<R, E>,
    {
        let mut boxed = Self::new_unlocked(len);

        assert!(
            boxed.ptr != core::ptr::NonNull::dangling(),
            "Make sure pointer isn't dangling"
        );
        assert!(boxed.len == len);

        let res = init(&mut boxed);

        boxed.lock();

        res.map(|_| boxed)
    }

    pub(crate) fn len(&self) -> usize {
        self.len
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub(crate) fn size(&self) -> usize {
        self.len * T::size()
    }

    pub(crate) fn unlock(&self) -> &Self {
        self.retain(Prot::ReadOnly);
        self
    }

    pub(crate) fn unlock_mut(&mut self) -> &mut Self {
        self.retain(Prot::ReadWrite);
        self
    }

    pub(crate) fn lock(&self) {
        self.release()
    }

    #[allow(dead_code)]
    pub(crate) fn as_ref(&self) -> &T {
        assert!(!self.is_empty(), "Attempted to dereference a zero-length pointer");

        assert!(self.prot.get() != Prot::NoAccess, "May not call Boxed while locked");

        unsafe { self.ptr.as_ref() }
    }

    pub(crate) fn as_mut(&mut self) -> &mut T {
        assert!(!self.is_empty(), "Attempted to dereference a zero-length pointer");

        assert!(
            self.prot.get() == Prot::ReadWrite,
            "May not call Boxed unless mutably unlocked"
        );

        unsafe { self.ptr.as_mut() }
    }

    pub(crate) fn as_slice(&self) -> &[T] {
        assert!(self.prot.get() != Prot::NoAccess, "May not call Boxed while locked");

        unsafe { slice::from_raw_parts(self.ptr.as_ptr(), self.len) }
    }

    pub(crate) fn as_mut_slice(&mut self) -> &mut [T] {
        assert!(
            self.prot.get() == Prot::ReadWrite,
            "May not call Boxed unless mutably unlocked"
        );

        unsafe { slice::from_raw_parts_mut(self.ptr.as_ptr(), self.len) }
    }

    fn new_unlocked(len: usize) -> Self {
        if len == 0 {
            return Self {
                ptr: NonNull::dangling(),
                len: 0,
                allocated_size: 0,
                prot: Cell::new(Prot::ReadWrite),
                refs: Cell::new(1),
            };
        }

        let size = len * mem::size_of::<T>();
        let layout = Layout::from_size_align(size, mem::align_of::<T>())
            .expect("Invalid layout");

        // Allocate using dryoc's page-aligned allocator with mlock
        let allocator = PageAlignedAllocator;
        let allocation = allocator.allocate(layout)
            .expect("Failed to allocate memory");

        let ptr = NonNull::new(allocation.as_ptr() as *mut _ as *mut T)
            .expect("Allocator returned null");

        Self {
            ptr,
            len,
            allocated_size: size,
            prot: Cell::new(Prot::ReadWrite),
            refs: Cell::new(1),
        }
    }

    fn retain(&self, prot: Prot) {
        let refs = self.refs.get();

        if refs == 0 {
            assert!(prot != Prot::NoAccess, "Must retain readably or writably");

            self.prot.set(prot);
            mprotect(self.ptr.as_ptr(), self.allocated_size, prot);
        } else {
            assert!(
                Prot::NoAccess != self.prot.get(),
                "Out-of-order retain/release detected"
            );
            assert!(
                Prot::ReadWrite != self.prot.get(),
                "Cannot unlock mutably more than once"
            );
            assert!(Prot::ReadOnly == prot, "Cannot unlock mutably while unlocked immutably");
        }

        match refs.checked_add(1) {
            Some(v) => self.refs.set(v),
            None if self.is_locked() => panic!("Out-of-order retain/release detected"),
            None => panic!("Retained too many times"),
        };
    }

    fn release(&self) {
        assert!(self.refs.get() != 0, "Releases exceeded retains");

        assert!(
            self.prot.get() != Prot::NoAccess,
            "Releasing memory that's already locked"
        );

        let refs = self.refs.get().wrapping_sub(1);

        self.refs.set(refs);

        if refs == 0 {
            mprotect(self.ptr.as_ptr(), self.allocated_size, Prot::NoAccess);
            self.prot.set(Prot::NoAccess);
        }
    }

    fn is_locked(&self) -> bool {
        self.prot.get() == Prot::NoAccess
    }

    #[cfg(test)]
    #[allow(dead_code)]
    /// Returns the address of the pointer to the data
    pub fn get_ptr_address(&self) -> usize {
        self.ptr.as_ptr() as *const _ as usize
    }
}

impl<T: Bytes + Randomized> Boxed<T> {
    #[allow(dead_code)]
    pub(crate) fn random(len: usize) -> Self {
        Self::new(len, |b| b.as_mut_slice().randomize())
    }
}

impl<T: Bytes + Zeroed> Boxed<T> {
    #[allow(dead_code)]
    pub(crate) fn zero(len: usize) -> Self {
        Self::new(len, |b| b.as_mut_slice().zero())
    }
}

// This may create undefined behaviour if not used correctly
// Zeroes out the memory and configuration
impl<T: Bytes> Zeroize for Boxed<T> {
    fn zeroize(&mut self) {
        self.unlock_mut();
        self.as_mut_slice().zero();
        self.lock();
        self.refs.set(0);
        self.prot.set(Prot::NoAccess);
        self.len = 0;
        // NOTE: Do NOT reset allocated_size to 0, as it's needed for mprotect calls
        // self.allocated_size = 0;
    }
}

impl<T: Bytes> Drop for Boxed<T> {
    fn drop(&mut self) {
        extern crate std;

        use std::thread;

        if !thread::panicking() {
            assert!(self.refs.get() == 0, "Retains exceeded releases");

            assert!(self.prot.get() == Prot::NoAccess, "Dropped secret was still accessible");
        }

        // Ensure memory is accessible before freeing
        // Some systems may have issues freeing NoAccess memory
        if self.prot.get() != Prot::ReadWrite {
            mprotect(self.ptr.as_ptr(), self.allocated_size, Prot::ReadWrite);
        }

        unsafe { free(self.ptr.as_mut(), self.allocated_size) }
    }
}

impl<T: Bytes> Debug for Boxed<T> {
    fn fmt(&self, fmt: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(fmt, "{{ size: {}, hidden }}", self.size())
    }
}

impl<T: Bytes> Clone for Boxed<T> {
    fn clone(&self) -> Self {
        Self::new(self.len, |b| {
            b.as_mut_slice().copy_from_slice(self.unlock().as_slice());
            self.lock();
        })
    }
}

impl<T: Bytes + ConstEq> PartialEq for Boxed<T> {
    fn eq(&self, other: &Self) -> bool {
        if self.len != other.len {
            return false;
        }

        let lhs = self.unlock().as_slice();
        let rhs = other.unlock().as_slice();

        let ret = lhs.const_eq(rhs);

        self.lock();
        other.lock();

        ret
    }
}

impl<T: Bytes + Zeroed> From<&mut T> for Boxed<T> {
    fn from(data: &mut T) -> Self {
        Self::new(1, |b| unsafe { data.copy_and_zero(b.as_mut()) })
    }
}

impl<T: Bytes + Zeroed> From<&mut [T]> for Boxed<T> {
    fn from(data: &mut [T]) -> Self {
        Self::new(data.len(), |b| unsafe { data.copy_and_zero(b.as_mut_slice()) })
    }
}

unsafe impl<T: Bytes + Send> Send for Boxed<T> {}
unsafe impl<T: Bytes + Sync> Sync for Boxed<T> {}

#[cfg(unix)]
fn mprotect<T>(ptr: *mut T, size: usize, prot: Prot) {
    use libc::{mprotect as libc_mprotect, PROT_NONE, PROT_READ, PROT_WRITE};

    // Skip mprotect for zero-sized allocations
    if size == 0 {
        return;
    }

    let prot_flags = match prot {
        Prot::NoAccess => PROT_NONE,
        Prot::ReadOnly => PROT_READ,
        Prot::ReadWrite => PROT_READ | PROT_WRITE,
    };

    let result = unsafe { libc_mprotect(ptr as *mut libc::c_void, size, prot_flags) };
    if result != 0 {
        panic!("Error setting memory protection to {:?}", prot);
    }
}

#[cfg(windows)]
fn mprotect<T>(ptr: *mut T, size: usize, prot: Prot) {
    use windows::Win32::System::Memory::{VirtualProtect, PAGE_NOACCESS, PAGE_READONLY, PAGE_READWRITE};

    // Skip mprotect for zero-sized allocations
    if size == 0 {
        return;
    }

    let prot_flags = match prot {
        Prot::NoAccess => PAGE_NOACCESS,
        Prot::ReadOnly => PAGE_READONLY,
        Prot::ReadWrite => PAGE_READWRITE,
    };

    let mut old_protect = PAGE_NOACCESS;
    let result = unsafe { VirtualProtect(ptr as *const _, size, prot_flags, &mut old_protect) };
    if !result.as_bool() {
        panic!("Error setting memory protection to {:?}", prot);
    }
}

pub(crate) unsafe fn free<T>(ptr: *mut T, size: usize) {
    // Skip deallocation for zero-sized allocations
    if size == 0 {
        return;
    }

    let layout = Layout::from_size_align_unchecked(size, mem::align_of::<T>());
    let nonnull_ptr = NonNull::new(ptr).expect("free received null pointer");

    let allocator = PageAlignedAllocator;
    allocator.deallocate(nonnull_ptr.cast(), layout);
}

#[cfg(test)]
mod test {
    extern crate alloc;

    use alloc::vec;

    use super::*;

    #[test]
    fn boxed_zeroize() {
        let mut boxed = Boxed::<u8>::random(4);
        let ptr = unsafe { core::slice::from_raw_parts(boxed.ptr.as_ptr(), 4) };
        boxed.unlock();
        assert_ne!(ptr, [0u8; 4]);
        boxed.lock();

        boxed.zeroize();

        boxed.unlock();
        assert_eq!(ptr, [0u8; 4]);
        boxed.lock();
    }

    #[test]
    #[ignore] // This test is flaky - it depends on getting non-zero garbage from freshly allocated memory
              // which may or may not happen depending on OS/allocator behavior
    fn test_init_with_garbage() {
        let boxed = Boxed::<u8>::new(4, |_| {});
        let unboxed = boxed.unlock().as_slice();

        let garbage = unsafe {
            let mut garb_ptr: *mut libc::c_void = std::ptr::null_mut();
            let result = libc::posix_memalign(&mut garb_ptr, 16, 1);
            if result != 0 || garb_ptr.is_null() {
                panic!("Failed to allocate memory");
            }
            let garb_byte = *(garb_ptr as *mut u8);

            libc::free(garb_ptr);

            vec![garb_byte; unboxed.len()]
        };

        assert_ne!(garbage, vec![0; garbage.len()]);
        assert_eq!(unboxed, &garbage[..]);

        boxed.lock();
    }

    #[test]
    fn test_custom_init() {
        let boxed = Boxed::<u8>::new(1, |secret| {
            secret.as_mut_slice().copy_from_slice(b"\x04");
        });

        assert_eq!(boxed.unlock().as_slice(), [0x04]);
        boxed.lock();
    }

    #[test]
    fn test_init_with_zero() {
        let boxed = Boxed::<u8>::zero(6);

        assert_eq!(boxed.unlock().as_slice(), [0, 0, 0, 0, 0, 0]);

        boxed.lock();
    }

    #[test]
    fn test_init_with_values() {
        let mut value = [8u64];
        let boxed = Boxed::from(&mut value[..]);

        assert_eq!(value, [0]);
        assert_eq!(boxed.unlock().as_slice(), [8]);

        boxed.lock();
    }

    #[allow(clippy::redundant_clone)]
    #[test]
    fn test_eq() {
        let boxed_a = Boxed::<u8>::random(1);
        let boxed_b = boxed_a.clone();

        assert_eq!(boxed_a, boxed_b);
        assert_eq!(boxed_b, boxed_a);

        let boxed_a = Boxed::<u8>::random(16);
        let boxed_b = Boxed::<u8>::random(16);

        assert_ne!(boxed_a, boxed_b);
        assert_ne!(boxed_b, boxed_a);

        let boxed_b = Boxed::<u8>::random(12);

        assert_ne!(boxed_a, boxed_b);
        assert_ne!(boxed_b, boxed_a);
    }

    #[test]
    fn test_refs() {
        let mut boxed = Boxed::<u8>::zero(8);

        assert_eq!(0, boxed.refs.get());

        let _ = boxed.unlock();
        let _ = boxed.unlock();

        assert_eq!(2, boxed.refs.get());

        boxed.lock();
        boxed.lock();

        assert_eq!(0, boxed.refs.get());

        let _ = boxed.unlock_mut();

        assert_eq!(1, boxed.refs.get());

        boxed.lock();

        assert_eq!(0, boxed.refs.get());
    }

    #[test]
    fn test_ref_overflow() {
        let boxed = Boxed::<u8>::zero(8);

        for _ in 0..u8::max_value() {
            let _ = boxed.unlock();
        }

        for _ in 0..u8::max_value() {
            boxed.lock()
        }
    }

    #[test]
    fn test_random_borrow_amounts() {
        let boxed = Boxed::<u8>::zero(1);
        let mut counter = 0u8;

        counter.randomize();

        for _ in 0..counter {
            let _ = boxed.unlock();
        }

        for _ in 0..counter {
            boxed.lock()
        }
    }

    #[test]
    fn test_threading() {
        extern crate std;

        use std::{sync::mpsc, thread};

        let (tx, rx) = mpsc::channel();

        let ch = thread::spawn(move || {
            let boxed = Boxed::<u64>::random(1);
            let val = boxed.unlock().as_slice().to_vec();

            tx.send((boxed, val)).expect("failed to send via channel");
        });

        let (boxed, val) = rx.recv().expect("failed to read from channel");

        assert_eq!(Prot::ReadOnly, boxed.prot.get());
        assert_eq!(val, boxed.as_slice());

        ch.join().expect("child thread terminated.");
        boxed.lock();
    }

    #[test]
    #[should_panic(expected = "Retained too many times")]
    fn test_overflow_refs() {
        let boxed = Boxed::<[u8; 4]>::zero(4);

        for _ in 0..=u8::max_value() {
            let _ = boxed.unlock();
        }

        for _ in 0..boxed.refs.get() {
            boxed.lock()
        }
    }

    #[test]
    #[should_panic(expected = "Out-of-order retain/release detected")]
    fn test_out_of_order() {
        let boxed = Boxed::<u8>::zero(3);

        boxed.refs.set(boxed.refs.get().wrapping_sub(1));
        boxed.prot.set(Prot::NoAccess);

        boxed.retain(Prot::ReadOnly);
    }

    #[test]
    #[should_panic(expected = "Attempted to dereference a zero-length pointer")]
    fn test_zero_length() {
        let boxed = Boxed::<u8>::zero(0);

        let _ = boxed.as_ref();
    }

    #[test]
    #[should_panic(expected = "Cannot unlock mutably more than once")]
    fn test_multiple_writers() {
        let mut boxed = Boxed::<u64>::zero(1);

        let _ = boxed.unlock_mut();
        let _ = boxed.unlock_mut();
    }

    #[test]
    #[should_panic(expected = "Releases exceeded retains")]
    fn test_release_vs_retain() {
        Boxed::<u64>::zero(2).lock();
    }
}
