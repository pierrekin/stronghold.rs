// Copyright 2020-2021 IOTA Stiftung
// SPDX-License-Identifier: Apache-2.0

use crate::types::*;

use subtle::ConstantTimeEq;

/// A trait for comparing types in Constant Time using [`subtle::ConstantTimeEq`].
pub trait ConstEq: ContiguousBytes {
    fn const_eq(&self, rhs: &Self) -> bool {
        self.as_bytes().ct_eq(rhs.as_bytes()).into()
    }
}

impl<T: ContiguousBytes> ConstEq for T {}
impl<T: Bytes> ConstEq for [T] {}
