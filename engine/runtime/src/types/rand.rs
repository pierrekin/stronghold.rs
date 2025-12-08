// Copyright 2020-2021 IOTA Stiftung
// SPDX-License-Identifier: Apache-2.0

use crate::types::*;

use dryoc::rng::copy_randombytes;

/// A trait for generating random bytes via [`dryoc::rng::copy_randombytes`].
pub unsafe trait Randomized: ContiguousBytes {
    fn randomize(&mut self) {
        copy_randombytes(self.as_mut_bytes());
    }
}

unsafe impl<T: ContiguousBytes + ?Sized> Randomized for T {}
