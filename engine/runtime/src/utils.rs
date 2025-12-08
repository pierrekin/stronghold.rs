// Copyright 2020-2021 IOTA Stiftung
// SPDX-License-Identifier: Apache-2.0

use dryoc::rng;
use zeroize::Zeroizing;

pub fn xor(data: &mut [u8], payload: &[u8], noise: &[u8], size: usize) {
    for i in 0..size {
        data[i] = noise[i] ^ payload[i];
    }
}

pub fn xor_mut(payload: &mut [u8], noise: &[u8], size: usize) {
    for i in 0..size {
        payload[i] ^= noise[i];
    }
}

pub fn random_vec(size: usize) -> Zeroizing<Vec<u8>> {
    let mut v = Zeroizing::new(vec![0u8; size]);
    rng::copy_randombytes(&mut v);
    v
}

// Creates random file name and join it to the storing directory
pub fn random_fname(size: usize) -> String {
    const ALPHANUMERIC: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    let random_bytes = rng::randombytes_buf(size);
    random_bytes
        .iter()
        .map(|&b| ALPHANUMERIC[(b as usize) % ALPHANUMERIC.len()] as char)
        .collect()
}
