extern crate amcl;
extern crate hex;
extern crate rand;
extern crate ring;

use self::amcl::arch::Chunk;
use self::ring::digest::{digest, SHA256};
use super::errors::DecodeError;
use super::fouque_tibouchi::{fouque_tibouchi_g1, fouque_tibouchi_g2, fouque_tibouchi_twice_g1, fouque_tibouchi_twice_g2};
use super::optimised_swu::{optimised_swu_g2, optimised_swu_g2_twice};
use super::psi_cofactor::clear_g2_psi;
use BLSCurve::big::{Big, MODBYTES, NLEN};
use BLSCurve::dbig::DBig;
use BLSCurve::ecp::ECP;
use BLSCurve::ecp2::ECP2;
use BLSCurve::fp::FP as bls381_FP;
use BLSCurve::fp12::FP12 as bls381_FP12;
use BLSCurve::fp2::FP2 as bls381_FP2;
use BLSCurve::pair::{ate, fexp};
use BLSCurve::rom;

pub type DBigNum = DBig;
pub type BigNum = Big;
pub type GroupG1 = ECP;
pub type GroupG2 = ECP2;

pub type FP = bls381_FP;
pub type FP2 = bls381_FP2;
pub type FP12 = bls381_FP12;

pub const CURVE_ORDER: [Chunk; NLEN] = rom::CURVE_ORDER;

// Byte size of element in group G1
pub const G1_BYTE_SIZE: usize = (2 * MODBYTES) as usize;
// Byte size of element in group G2
pub const G2_BYTE_SIZE: usize = (4 * MODBYTES) as usize;
// Byte size of secret key
pub const MOD_BYTE_SIZE: usize = MODBYTES;

// G2_Cofactor as arrays of i64
pub const G2_COFACTOR_HIGH: [Chunk; NLEN] = [
    0x0153_7E29_3A66_91AE,
    0x023C_72D3_67A0_BBC8,
    0x0205_B2E5_A7DD_FA62,
    0x0115_1C21_6AEA_9A28,
    0x0128_76A2_02CD_91DE,
    0x0105_39FC_4247_541E,
    0x0000_0000_5D54_3A95,
];
pub const G2_COFACTOR_LOW: [Chunk; NLEN] = [
    0x031C_38E3_1C72_38E5,
    0x01BB_1B9E_1BC3_1C33,
    0x0000_0000_0000_0161,
    0x0000_0000_0000_0000,
    0x0000_0000_0000_0000,
    0x0000_0000_0000_0000,
    0x0000_0000_0000_0000,
];
pub const G2_COFACTOR_SHIFT: [Chunk; NLEN] = [
    0x0000_0000_0000_0000,
    0x0000_0000_0000_0000,
    0x0000_0000_0000_1000,
    0x0000_0000_0000_0000,
    0x0000_0000_0000_0000,
    0x0000_0000_0000_0000,
    0x0000_0000_0000_0000,
];

// Hash Constants
pub const HASH_REPS: u8 = 2;

#[cfg(feature = "std")]
lazy_static! {
    // Generators
    pub static ref GENERATORG1: GroupG1 = GroupG1::generator();
    pub static ref GENERATORG2: GroupG2 = GroupG2::generator();
}

// Take given message and domain and convert it to GroupG2 point
pub fn hash_on_g2(msg: &[u8], domain: u64) -> GroupG2 {
    // This is a wrapper to easily change implementation if we switch hashing methods
    hash_and_test_g2(msg, domain)
}

// Convert a message to a Fp point
//
// https://github.com/pairingwg/bls_standard/blob/master/minutes/spec-v1.md
pub fn hash_to_field_g1(msg: &[u8], ctr: u8) ->  FP {
    // Values to be combined as FP
    let mut t: Vec<u8> = vec![];
    for j in 1 ..= HASH_REPS {
        // As SHA256 is 32 bytes and p ~48 bytes, hash twice and concatenate
        t.append(&mut hash(&[msg, &[ctr, 0, j]].concat()));
    }

    // Increase length of 't' to size of DBig (96 bytes)
    for _ in t.len() .. MODBYTES * 2 {
        t.push(0);
    }

    // Modulate the t by p
    let mut dbig_t = DBig::frombytes(&t);
    let p = BigNum::new_ints(&rom::MODULUS);
    let e = dbig_t.dmod(&p);

    FP::new_big(&e)
}

// Convert a message to a Fp2 point
//
// https://github.com/pairingwg/bls_standard/blob/master/minutes/spec-v1.md
pub fn hash_to_field_g2(msg: &[u8], ctr: u8) ->  FP2 {
    // Values to be combined as FP2
    let mut e = [BigNum::new(); 2];
    // Loop twice as two FP values are required in Fp2
    for i in 1 ..= 2 {
        let mut t: Vec<u8> = vec![];
        for j in 1 ..= HASH_REPS {
            // As SHA256 is 32 bytes and p ~48 bytes, hash twice and concatenate
            t.append(&mut hash(&[msg, &[ctr, i, j]].concat()));
        }

        // Increase t to size of DBig (96 bytes)
        let mut buf = vec![0; MODBYTES * 2 - t.len()];
        buf.append(&mut t);

        // Modulate the t by p
        let mut dbig_t = DBig::frombytes(&buf);
        let p = BigNum::new_ints(&rom::MODULUS);
        e[(i - 1) as usize] = dbig_t.dmod(&p);
    }
    FP2::new_bigs(&e[0], &e[1])
}

// Clear the G2 cofactor
//
// This is a wrapper function to enable easy switching between multiplication by cofctor
// and
pub fn clear_g2_cofactor(point: &mut GroupG2) -> GroupG2 {
    clear_g2_psi(point)
}

// Multiply in parts by cofactor due to its size.
pub fn multiply_g2_cofactor(point: &mut GroupG2) -> GroupG2 {
    // Replicate curve_point for low part of multiplication
    let mut lowpart = GroupG2::new();
    lowpart.copy(&point);

    // Convert const arrays to BigNums
    let g2_cofactor_high = BigNum::new_ints(&G2_COFACTOR_HIGH);
    let g2_cofactor_shift = BigNum::new_ints(&G2_COFACTOR_SHIFT);
    let g2_cofactor_low = BigNum::new_ints(&G2_COFACTOR_LOW);

    // Multiply high part, then low part, then add together
    let mut point = point.mul(&g2_cofactor_high);
    point = point.mul(&g2_cofactor_shift);
    let lowpart = lowpart.mul(&g2_cofactor_low);
    point.add(&lowpart);
    point
}

// Provides a Keccak256 hash of given input.
pub fn hash(input: &[u8]) -> Vec<u8> {
    digest(&SHA256, input).as_ref().into()
}

// A pairing function for an GroupG2 point and GroupG1 point to FP12.
pub fn ate_pairing(point_g2: &GroupG2, point_g1: &GroupG1) -> FP12 {
    let e = ate(&point_g2, &point_g1);
    fexp(&e)
}

// Take a GroupG1 point (x, y) and compress it to a 384 bit array.
pub fn compress_g1(g1: &mut GroupG1) -> Vec<u8> {
    // A compressed point takes form (c_flag, b_flag, a_flag, x-coordinate) where:
    // c_flag == 1
    // b_flag represents infinity (1 if infinitity -> x = y = 0)
    // a_flag = y % 2 (i.e. odd or eveness of y point)
    // x is the x-coordinate of

    // Check point at inifinity
    if g1.is_infinity() {
        let mut result: Vec<u8> = vec![0; MODBYTES];
        // Set b_flag and c_flag to 1, all else to 0
        result[0] = u8::pow(2, 6) + u8::pow(2, 7);
        return result;
    }

    // Convert point to array of bytes (x, y)
    let mut g1_bytes: Vec<u8> = vec![0; G1_BYTE_SIZE + 1];
    g1.tobytes(&mut g1_bytes, false);

    // Convert arrary (x, y) to compressed format
    let mut result: Vec<u8> = vec![0; MODBYTES];
    result.copy_from_slice(&g1_bytes[1..=MODBYTES]); // byte[0] is Milagro formatting

    // Set flags
    let a_flag = calc_a_flag(&BigNum::frombytes(&g1_bytes[MODBYTES + 1..]));
    result[0] += u8::pow(2, 5) * a_flag; // set a_flag
    result[0] += u8::pow(2, 7); // c_flag

    result
}

// Take a 384 bit array and convert to GroupG1 point (x, y)
pub fn decompress_g1(g1_bytes: &[u8]) -> Result<GroupG1, DecodeError> {
    // Length must be 48 bytes
    if g1_bytes.len() != MODBYTES {
        return Err(DecodeError::IncorrectSize);
    }

    let a_flag: u8 = g1_bytes[0] % u8::pow(2, 6) / u8::pow(2, 5);

    // c_flag must be set
    if g1_bytes[0] / u8::pow(2, 7) != 1 {
        // Invalid bytes
        return Err(DecodeError::InvalidCFlag);
    }

    // Check b_flag
    if g1_bytes[0] % u8::pow(2, 7) / u8::pow(2, 6) == 1 {
        // If b_flag == 1 -> a_flag == x == 0
        if a_flag != 0 || g1_bytes[0] % u8::pow(2, 5) != 0 {
            return Err(DecodeError::BadPoint);
        }

        for item in g1_bytes.iter().skip(1) {
            if *item != 0 {
                return Err(DecodeError::BadPoint);
            }
        }

        // Point is infinity
        return Ok(GroupG1::new());
    }

    let mut g1_bytes = g1_bytes.to_owned();

    // Zero remaining flags so it can be converted to 381 bit BigNum
    g1_bytes[0] %= u8::pow(2, 5);
    let x_big = BigNum::frombytes(&g1_bytes);

    // Convert to GroupG1 point using big
    let mut point = GroupG1::new_big(&x_big);
    if point.is_infinity() {
        return Err(DecodeError::BadPoint);
    }

    // Confirm a_flag
    let calculated_a_flag = calc_a_flag(&point.gety());
    if calculated_a_flag != a_flag {
        point.neg();
    }

    Ok(point)
}

// Take a GroupG2 point (x, y) and compress it to a 384*2 bit array.
pub fn compress_g2(g2: &mut GroupG2) -> Vec<u8> {
    // A compressed point takes form:
    // (c_flag1, b_flag1, a_flag1, x-coordinate.a, 0, 0, 0, x-coordinate.b) where:
    // c_flag1 == 1
    // b_flag1 represents infinity (1 if infinitity -> x = y = 0)
    // a_flag1 = y_imaginary % 2 (i.e. point.gety().getb())
    // x is the x-coordinate of

    // Check point at inifinity
    if g2.is_infinity() {
        let mut result: Vec<u8> = vec![0; G2_BYTE_SIZE / 2];
        // Set b_flag and c_flag to 1, all else to 0
        result[0] += u8::pow(2, 6) + u8::pow(2, 7);
        return result;
    }

    // Convert point to array of bytes (x, y)
    let mut g2_bytes: Vec<u8> = vec![0; G2_BYTE_SIZE];
    g2.tobytes(&mut g2_bytes);

    // Convert arrary (x, y) to compressed format
    // Note: amcl is x(re, im), y(re, im) eth is x(im, re), y(im, re)
    let x_real = &g2_bytes[0..MODBYTES];
    let x_imaginary = &g2_bytes[MODBYTES..(MODBYTES * 2)];
    let mut result: Vec<u8> = vec![0; MODBYTES];
    result.copy_from_slice(x_imaginary);
    result.extend_from_slice(x_real);

    // Set flags
    let a_flag = calc_a_flag(&BigNum::frombytes(&g2_bytes[MODBYTES * 3..]));
    result[0] += u8::pow(2, 5) * a_flag;
    result[0] += u8::pow(2, 7); // c_flag

    result
}

// Take a 384*2 bit array and convert to GroupG2 point (x, y)
pub fn decompress_g2(g2_bytes: &[u8]) -> Result<GroupG2, DecodeError> {
    // Length must be 96 bytes
    if g2_bytes.len() != G2_BYTE_SIZE / 2 {
        return Err(DecodeError::IncorrectSize);
    }

    // c_flag must be set
    if g2_bytes[0] / u8::pow(2, 7) != 1 {
        // Invalid bytes
        return Err(DecodeError::InvalidCFlag);
    }

    // Check b_flag
    if g2_bytes[0] % u8::pow(2, 7) / u8::pow(2, 6) == 1 {
        // If b_flag == 1 -> a_flag == x == 0
        if g2_bytes[0] % u8::pow(2, 6) != 0 {
            return Err(DecodeError::BadPoint);
        }

        for item in g2_bytes.iter().skip(1) {
            if *item != 0 {
                return Err(DecodeError::BadPoint);
            }
        }
        // Point is infinity
        return Ok(GroupG2::new());
    }

    let a_flag: u8 = g2_bytes[0] % u8::pow(2, 6) / u8::pow(2, 5);

    let mut g2_bytes = g2_bytes.to_owned();

    // Zero remaining flags so it can be converted to 381 bit BigNum
    g2_bytes[0] %= u8::pow(2, 5);

    // Convert from array to FP2
    let x_imaginary = BigNum::frombytes(&g2_bytes[0..MODBYTES]);
    let x_real = BigNum::frombytes(&g2_bytes[MODBYTES..]);
    let x = FP2::new_bigs(&x_real, &x_imaginary);

    // Convert to GroupG1 point using big and sign
    let mut point = GroupG2::new_fp2(&x);
    if point.is_infinity() {
        return Err(DecodeError::BadPoint);
    }

    // Confirm a_flag matches given flag
    let calculated_a_flag = calc_a_flag(&point.gety().getb());
    if calculated_a_flag != a_flag {
        point.neg();
    }

    Ok(point)
}

// Takes a y-value and calculates if a_flag is 1 or 0
//
// a_flag = floor((y * 2)  / p)
pub fn calc_a_flag(y: &BigNum) -> u8 {
    let mut y2 = *y;
    y2.imul(2);
    let p = BigNum::new_ints(&rom::MODULUS);

    // if y * 2 < p => floor(y * 2 / p) = 0
    if BigNum::comp(&y2, &p) < 0 {
        return 0;
    }

    1
}

/**********************
* Hash and Test Methods
**********************/

// Use hash-and-test method to convert a hash to a G1 point
pub fn hash_and_test_g1(msg: &[u8], domain: u64) -> GroupG1 {
    // Counter for incrementing the pre-hash messages
    let mut counter = 0 as u8;
    let mut curve_point: GroupG1;
    let p = BigNum::new_ints(&rom::MODULUS);

    // Continue to increment pre-hash message until valid x coordinate is found
    loop {
        // Hash (message, domain, counter) for x coordinate
        let mut x = vec![0 as u8; 16];
        x.append(&mut hash(
            &[msg, &domain.to_be_bytes(), &[counter]].concat(),
        ));

        // Convert Hashes to BigNums mod p
        let mut x = BigNum::frombytes(&x);
        x.rmod(&p);

        curve_point = GroupG1::new_big(&x);

        if !curve_point.is_infinity() {
            break;
        }

        counter += 1;
    }

    // Take larger of two y values
    let mut y = curve_point.getpy();
    if y.is_neg() {
        // y is negative if y > -y
        curve_point.neg();
    }

    // Multiply the point by given G1_Cofactor
    curve_point.cfp(); // TODO: ensure this is correct G1 cofactor
    curve_point
}

// Use hash-and-test method to convert a Hash to a G2 point
#[allow(non_snake_case)]
pub fn hash_and_test_g2(msg: &[u8], domain: u64) -> GroupG2 {
    // Counter for incrementing the pre-hash messages
    let mut real_counter = 1 as u8;
    let mut imaginary_counter = 2 as u8;
    let mut curve_point: GroupG2;

    // Continue to increment pre-hash message until valid x coordinate is found
    loop {
        // Hash (message, domain, counter) for x-real and x-imaginary
        let mut x_real = vec![0 as u8; 16];
        x_real.append(&mut hash(
            &[msg, &domain.to_be_bytes(), &[real_counter]].concat(),
        ));
        let mut x_imaginary = vec![0 as u8; 16];
        x_imaginary.append(&mut hash(
            &[msg, &domain.to_be_bytes(), &[imaginary_counter]].concat(),
        ));

        // Convert Hashes to Fp2
        let x_real = BigNum::frombytes(&x_real);
        let x_imaginary = BigNum::frombytes(&x_imaginary);
        let mut x = FP2::new_bigs(&x_real, &x_imaginary);

        x.norm();
        curve_point = GroupG2::new_fp2(&x);

        if !curve_point.is_infinity() {
            break;
        }

        real_counter += 1;
        imaginary_counter += 1;
    }

    // Take larger of two y values
    let mut y = curve_point.getpy();
    if y.is_neg() {
        // y is negative if y > -y
        curve_point.neg();
    }

    // Multiply the point by given G2_Cofactor
    clear_g2_cofactor(&mut curve_point)
}

#[cfg(test)]
mod tests {
    extern crate yaml_rust;

    use self::yaml_rust::yaml;
    use super::*;
    use std::{fs::File, io::prelude::*, path::PathBuf};

    #[test]
    fn compression_decompression_g1_round_trip() {
        // Input 1
        let compressed = hex::decode("b53d21a4cfd562c469cc81514d4ce5a6b577d8403d32a394dc265dd190b47fa9f829fdd7963afdf972e5e77854051f6f").unwrap();
        let mut decompressed = decompress_g1(&compressed).unwrap();
        let compressed_result = compress_g1(&mut decompressed);
        assert_eq!(compressed, compressed_result);

        // Input 2
        let compressed = hex::decode("b301803f8b5ac4a1133581fc676dfedc60d891dd5fa99028805e5ea5b08d3491af75d0707adab3b70c6a6a580217bf81").unwrap();
        let mut decompressed = decompress_g1(&compressed).unwrap();
        let compressed_result = compress_g1(&mut decompressed);
        assert_eq!(compressed, compressed_result);

        // Input 3
        let compressed = hex::decode("a491d1b0ecd9bb917989f0e74f0dea0422eac4a873e5e2644f368dffb9a6e20fd6e10c1b77654d067c0618f6e5a7f79a").unwrap();
        let mut decompressed = decompress_g1(&compressed).unwrap();
        let compressed_result = compress_g1(&mut decompressed);
        assert_eq!(compressed, compressed_result);
    }

    #[test]
    fn test_to_from_infinity_g1() {
        let mut point = GroupG1::new();
        let compressed = compress_g1(&mut point);
        let mut round_trip_point = decompress_g1(&compressed).unwrap();
        assert_eq!(point.tostring(), round_trip_point.tostring());
    }

    #[test]
    fn test_to_from_infinity_g2() {
        let mut point = GroupG2::new();
        let compressed = compress_g2(&mut point);
        let mut round_trip_point = decompress_g2(&compressed).unwrap();
        assert_eq!(point.tostring(), round_trip_point.tostring());
    }

    #[test]
    fn compression_decompression_g2_round_trip() {
        // Input 1
        let mut compressed_a = hex::decode("a666d31d7e6561371644eb9ca7dbcb87257d8fd84a09e38a7a491ce0bbac64a324aa26385aebc99f47432970399a2ecb").unwrap();
        let mut compressed_b = hex::decode("0def2d4be359640e6dae6438119cbdc4f18e5e4496c68a979473a72b72d3badf98464412e9d8f8d2ea9b31953bb24899").unwrap();
        compressed_a.append(&mut compressed_b);

        let mut decompressed = decompress_g2(&compressed_a).unwrap();
        let compressed_result = compress_g2(&mut decompressed);
        assert_eq!(compressed_a, compressed_result);

        // Input 2
        let mut compressed_a = hex::decode("a63e88274adb7a98d112c16f7057f388786496c8f57e03ee9052b46b15eb0166645008f8cc929eb4475e386f3e6f1df8").unwrap();
        let mut compressed_b = hex::decode("1181e97fac61e371a22f34a4622f7e343ca0d99846b175a92ad1bf1df6fd4d0800e4edb7c2eb3d8437ed10cbc2d88823").unwrap();
        compressed_a.append(&mut compressed_b);

        let mut decompressed = decompress_g2(&compressed_a).unwrap();
        let compressed_result = compress_g2(&mut decompressed);
        assert_eq!(compressed_a, compressed_result);

        // Input 3
        let mut compressed_a = hex::decode("b090fbc9d5c6c80fec73c567202a75664cd00c2592e472a4d81d2ed4b6a166311e809ca25eb88c5d0189cbf1baa8ea79").unwrap();
        let mut compressed_b = hex::decode("18ca20f0b66678c0230e65eb4ebb3d621940984f71eb5481453e4489dafcc7f6ee2c863b76671467002a8f2392063005").unwrap();
        compressed_a.append(&mut compressed_b);

        let mut decompressed = decompress_g2(&compressed_a).unwrap();
        let compressed_result = compress_g2(&mut decompressed);
        assert_eq!(compressed_a, compressed_result);
    }

    /*********************
     * Experimental Tests *
     **********************/
    #[test]
    fn test_hash_and_test_g1() {
        let msg = [1 as u8; 32];

        for i in 0..100 {
            assert!(!hash_and_test_g1(&msg, i).is_infinity());
        }
    }

    #[test]
    fn test_hash_and_test_g2() {
        let msg = [1 as u8; 32];

        for i in 0..100 {
            assert!(!hash_and_test_g2(&msg, i).is_infinity());
        }
    }

    #[test]
    fn test_hash_to_field() {
        let msg = hex::decode("821d8c1c38ad2f46081460330d07ddfd45b5d7cd6b324efb07b9365e4336427a").unwrap();
        println!("Len: {}", msg.len());
        let mut t0 = hash_to_field_g2(&msg, 0);
        let mut t1 = hash_to_field_g2(&msg, 1);
        println!("{}", t0.tostring());
        println!("{}", t1.tostring());
    }
}
