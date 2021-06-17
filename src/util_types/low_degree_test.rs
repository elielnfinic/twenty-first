use crate::shared_math::other::log_2;
use crate::shared_math::prime_field_element::{PrimeField, PrimeFieldElement};
use crate::shared_math::prime_field_polynomial::PrimeFieldPolynomial;
use crate::util_types::merkle_tree_vector::{MerkleTreeVector, Node};
use crate::utils::{get_index_from_bytes, get_n_hash_rounds};
use std::convert::TryInto;

pub fn verify(
    modulus: i128,
    s: usize,
    output: &[u8],
    codeword_size: usize,
    mut primitive_root_of_unity: i128,
) -> bool {
    let rounds_count_u16: u16 = bincode::deserialize(&output[0..2]).unwrap();
    let rounds_count: usize = rounds_count_u16 as usize;
    let field = PrimeField::new(modulus);
    let roots: Vec<[u8; 32]> = (0..rounds_count + 1)
        .map(|i| output[2 + i * 32..(i + 1) * 32 + 2].try_into().unwrap())
        .collect();
    println!("Last root: {:?}", roots.last().unwrap()); // TODO: REMOVE
    let challenge_hash_preimages: Vec<Vec<u8>> = (0..rounds_count)
        .map(|i| output[0..((i + 1) * 32 + 2)].to_vec())
        .collect();
    let challenge_hashes: Vec<[u8; 32]> = challenge_hash_preimages
        .iter()
        .map(|bs| *blake3::hash(bs.as_slice()).as_bytes())
        .collect();
    let challenges: Vec<i128> = challenge_hashes
        .iter()
        .map(|x| PrimeFieldElement::from_bytes_raw(&modulus, &x[0..16]))
        .collect();
    println!("challenges = {:?}", challenges);

    let partial_output = output[0..((rounds_count + 1) * 32 + 2)].to_vec();
    let mut number_of_leaves = codeword_size;
    let mut output_index: usize = (rounds_count + 1) * 32 + 2;
    let mut c_values: Vec<i128> = vec![];
    for i in 0..rounds_count {
        let mut hash_preimage: Vec<u8> = partial_output.clone();
        hash_preimage.push(i as u8);
        let hashes = get_n_hash_rounds(hash_preimage.as_slice(), s);
        let mut c_indices: Vec<usize> = vec![];
        let mut ab_indices: Vec<usize> = vec![];
        for hash in hashes.iter() {
            let c_index = get_index_from_bytes(&hash[0..16], number_of_leaves / 2);
            c_indices.push(c_index);
            let a_index = c_index;
            ab_indices.push(a_index);
            let b_index = c_index + number_of_leaves / 2;
            ab_indices.push(b_index);
        }
        number_of_leaves /= 2;
        println!("c_indices = {:?}", c_indices);

        let mut proof_size: u16 =
            bincode::deserialize(&output[output_index..output_index + 2]).unwrap();
        output_index += 2;
        let mut cursor = &output[output_index..output_index + proof_size as usize];
        let c_proofs: Vec<Vec<Option<Node<i128>>>> = bincode::deserialize_from(cursor).unwrap();
        c_values = c_proofs
            .iter()
            .map(|x| x[0].as_ref().unwrap().value.unwrap())
            .collect::<Vec<i128>>();
        println!("c_values = {:?}", c_values); // TODO: REMOVE
        output_index += proof_size as usize;
        proof_size = bincode::deserialize(&output[output_index..output_index + 2]).unwrap();
        output_index += 2;
        cursor = &output[output_index..output_index + proof_size as usize];
        let ab_proofs: Vec<Vec<Option<Node<i128>>>> = bincode::deserialize_from(cursor).unwrap();
        output_index += proof_size as usize;

        let valid_cs = MerkleTreeVector::verify_multi_proof(roots[i + 1], &c_indices, &c_proofs);
        let valid_abs = MerkleTreeVector::verify_multi_proof(roots[i], &ab_indices, &ab_proofs);
        if !valid_cs || !valid_abs {
            println!(
                "Found invalidity of indices on iteration {}: y = {}, s = {}",
                i, valid_cs, valid_abs
            );
            print!("Invalid proofs:");
            if !valid_abs {
                println!("{:?}", c_proofs);
            }
            if !valid_cs {
                println!("{:?}", ab_proofs);
            }
            return false;
        }

        let root = PrimeFieldElement::new(primitive_root_of_unity, &field);
        println!("primitive_root_of_unity = {}", primitive_root_of_unity);
        let challenge = challenges[i];
        for j in 0..s {
            let a_index = ab_indices[2 * j] as i128;
            let a_x = root.mod_pow_raw(a_index);
            let a_y = ab_proofs[2 * j][0].as_ref().unwrap().value.unwrap();
            let b_index = ab_indices[2 * j + 1] as i128;
            let b_x = root.mod_pow_raw(b_index);
            let b_y = ab_proofs[2 * j + 1][0].as_ref().unwrap().value.unwrap();
            // let c_index = c_indices[j] as i128;
            // let c_x = root.mod_pow_raw(c_index * 2);
            let c_y = c_proofs[j][0].as_ref().unwrap().value.unwrap();
            println!(
                "{{({},{}),({},{}),({},{})}}",
                a_x, a_y, b_x, b_y, challenge, c_y
            ); // TODO: REMOVE
            if !PrimeFieldPolynomial::are_colinear_raw(
                &[(a_x, a_y), (b_x, b_y), (challenge, c_y)],
                modulus,
            ) {
                println!("Failed to verify colinearity!");
                return false;
            } else {
                println!(
                    "({}, {}), ({}, {}), ({}, {}) are colinear",
                    a_x, a_y, b_x, b_y, challenge, c_y
                );
            }
        }

        primitive_root_of_unity = primitive_root_of_unity * primitive_root_of_unity % modulus;
    }

    // Base case: Verify that the last merkle tree is a constant function
    // Verify only the c indicies
    // let last_y_value =
    if !c_values.iter().all(|&x| c_values[0] == x) {
        println!("Last y values were not constant. Got: {:?}", c_values);
        return false;
    }

    true
}

pub fn fri_prover_iteration(
    codeword: &[i128],
    challenge: &i128,
    modulus: &i128,
    inv_two: &i128,
    primitive_root_of_unity: &i128,
) -> Vec<i128> {
    let mut new_codeword: Vec<i128> = vec![0i128; codeword.len() / 2];

    println!("challenge = {}", challenge);
    let mut x = 1i128;
    for i in 0..new_codeword.len() {
        // let (_, two_x_inv, _) = PrimeFieldElement::eea(2 * x, *modulus);
        let (_, x_inv, _) = PrimeFieldElement::eea(x, *modulus);
        // If codeword is the evaluation of a polynomial of degree N,
        // this is an evaluation of a polynomial of degree N/2
        new_codeword[i] = (((1 + challenge * x_inv) * codeword[i]
            + (1 - challenge * x_inv) * codeword[i + codeword.len() / 2])
            * *inv_two
            % *modulus
            + *modulus)
            % *modulus;
        // println!("codeword[i] = {}", codeword[i]); //
        // println!(
        //     "codeword[i + codeword.len() / 2] = {}",
        //     codeword[i + codeword.len() / 2]
        // );
        // let p_even = ((codeword[i] + codeword[i + codeword.len() / 2]) * *inv_two % *modulus
        //     + *modulus)
        //     % *modulus;
        // let p_odd = ((codeword[i] - codeword[i + codeword.len() / 2]) * two_x_inv % *modulus
        //     + *modulus)
        //     % *modulus;
        // println!("p_even = {}", p_even);
        // println!("p_odd = {}", p_odd);
        x = x * *primitive_root_of_unity % modulus;
    }
    println!("modulus = {}", modulus); // TODO: REMOVE
    println!("inv_two = {}", inv_two); // TODO: REMOVE
    println!("codeword: {:?}", codeword); // TODO: REMOVE
    println!("new_codeword: {:?}", new_codeword); // TODO: REMOVE
    new_codeword
}

// TODO: We want this implemented for prime field elements, and preferably for
// any finite field/extension field.
// Prove that codeword elements come from the evaluation of a polynomial of
// `degree < codeword.len() / rho`
pub fn prover(
    codeword: &[i128],
    modulus: i128,
    rho: usize,
    s: usize,
    output: &mut Vec<u8>,
    mut primitive_root_of_unity: i128, // TODO: REMOVE -- only used for debugging
) {
    let round_count = log_2((codeword.len() / rho) as u64) + 1;
    output.append(&mut bincode::serialize(&(round_count as u16)).unwrap());
    let mut mt = MerkleTreeVector::from_vec(codeword);
    output.append(&mut mt.get_root().to_vec());
    let mut mts: Vec<MerkleTreeVector<i128>> = vec![mt];
    let mut mut_codeword: Vec<i128> = codeword.to_vec().clone();

    // commit phase
    let (_, _, inv2_temp) = PrimeFieldElement::eea(modulus, 2);
    let inv2 = (inv2_temp + modulus) % modulus;
    let mut num_rounds = 0;
    let mut primitive_root_of_unity_temp = primitive_root_of_unity;
    while mut_codeword.len() >= rho {
        // get challenge
        let hash = *blake3::hash(output.as_slice()).as_bytes();
        let challenge: i128 = PrimeFieldElement::from_bytes_raw(&modulus, &hash[0..16]);

        // run fri iteration
        mut_codeword = fri_prover_iteration(
            &mut_codeword.clone(),
            &challenge,
            &modulus,
            &inv2,
            &primitive_root_of_unity_temp,
        );

        // wrap into merkle tree
        mt = MerkleTreeVector::from_vec(&mut_codeword);

        // append root to proof
        output.append(&mut mt.get_root().to_vec());

        // collect into memory
        mts.push(mt.clone());

        num_rounds += 1;
        primitive_root_of_unity_temp =
            primitive_root_of_unity_temp * primitive_root_of_unity_temp % modulus;
    }
    println!("last Merkle Tree: {:?}", mts[num_rounds]); // TODO: REMOVE

    // query phase
    // for all subsequent pairs of merkle trees:
    // - do s times:
    // -- sample random point y in L2
    // -- compute square roots s1 s2
    // -- query P1 in y -> beta
    // -- query P2 in s1 -> alpha1
    // -- query P2 in s2 -> alpha2
    // -- check collinearity (s0, alpha0), (s1, alpha1), (y, beta) <-- we don't care about thi right nw>
    let partial_output = output.clone();
    for i in 0usize..num_rounds {
        println!("i = {}", i);
        let number_of_leaves = mts[i].get_number_of_leafs();
        let mut c_indices: Vec<usize> = vec![];
        let mut ab_indices: Vec<usize> = vec![];

        // it's unrealistic that the number of rounds exceed 256 but this should wrap around if it does
        let mut hash_preimage: Vec<u8> = partial_output.clone();
        hash_preimage.push(i as u8);

        let hashes = get_n_hash_rounds(hash_preimage.as_slice(), s);
        for hash in hashes.iter() {
            let c_index = get_index_from_bytes(&hash[0..16], number_of_leaves / 2);
            c_indices.push(c_index);
            let s0_index = c_index;
            ab_indices.push(s0_index);
            let s1_index = c_index + number_of_leaves / 2;
            ab_indices.push(s1_index);
        }

        let authentication_paths_c: Vec<Vec<Option<Node<i128>>>> =
            mts[i + 1].get_multi_proof(&c_indices);
        let authentication_paths_ab: Vec<Vec<Option<Node<i128>>>> =
            mts[i].get_multi_proof(&ab_indices);

        // Debug, TODO: REMOVE
        if i >= num_rounds - 1 {
            println!(
                "i = {}, last Merkle Tree root: {:?}",
                i,
                mts[i + 1].get_root()
            ); // TODO: REMOVE
        }
        println!("c_indices = {:?}", c_indices);
        let field = PrimeField::new(modulus);
        let root = PrimeFieldElement::new(primitive_root_of_unity, &field);
        println!("number_of_leaves = {}", number_of_leaves);
        println!("c index = {}", c_indices[0]);
        println!("c x value = {}", root.mod_pow_raw(c_indices[0] as i128 * 2));
        println!(
            "c y value revealed = {}",
            authentication_paths_c[0][0]
                .as_ref()
                .unwrap()
                .value
                .unwrap()
        );
        println!("a index = {}", ab_indices[0]);
        println!("a x value = {}", root.mod_pow_raw(ab_indices[0] as i128));
        println!(
            "a y value revealed = {}",
            authentication_paths_ab[0][0]
                .as_ref()
                .unwrap()
                .value
                .unwrap()
        );
        println!("b index = {}", ab_indices[1]);
        println!("b x value = {}", root.mod_pow_raw(ab_indices[1] as i128));
        println!(
            "b y value revealed = {}",
            authentication_paths_ab[1][0]
                .as_ref()
                .unwrap()
                .value
                .unwrap()
        );

        // serialize proofs and store in output
        let mut c_paths_encoded = bincode::serialize(&authentication_paths_c.clone()).unwrap();
        output.append(&mut bincode::serialize(&(c_paths_encoded.len() as u16)).unwrap());
        output.append(&mut c_paths_encoded);

        let mut ab_paths_encoded = bincode::serialize(&authentication_paths_ab.clone()).unwrap();
        output.append(&mut bincode::serialize(&(ab_paths_encoded.len() as u16)).unwrap());
        output.append(&mut ab_paths_encoded);

        primitive_root_of_unity = primitive_root_of_unity * primitive_root_of_unity % modulus;
        // TODO: REMOVE -- only used for debugging
    }
}

#[cfg(test)]
mod test_utils {
    use super::*;
    use crate::shared_math::prime_field_element::PrimeField;

    #[test]
    fn generate_proof_small() {
        let mut ret: Option<(PrimeField, i128)> = None;
        PrimeField::get_field_with_primitive_root_of_unity(4, 100, &mut ret);
        assert_eq!(101i128, ret.clone().unwrap().0.q);
        let (field, primitive_root_of_unity) = ret.clone().unwrap();
        println!("prime = {}, root = {}", field.q, primitive_root_of_unity);
        let power_series = field.get_power_series(primitive_root_of_unity);
        assert_eq!(4, power_series.len());
        assert_eq!(vec![1i128, 10, 100, 91], power_series);
        let mut output = vec![];

        // corresponds to the polynomial P(x) = x
        // degree < codeword.len() / rho
        let y_values = power_series;
        let rho = 2;
        let s = 5;
        // prover(&y_values, field.q, rho, s, &mut output);
        prover(
            &y_values,
            field.q,
            rho,
            s,
            &mut output,
            primitive_root_of_unity,
        );
        println!("\n\n\n\n\n\n\n\n\n\n\n***************** PROOF DONE *****************");
        println!("***************** START VERIFY ***************** \n\n");
        assert!(verify(
            field.q,
            s,
            &output,
            y_values.len(),
            primitive_root_of_unity
        ));
    }

    #[test]
    fn generate_proof_parabola() {
        let mut ret: Option<(PrimeField, i128)> = None;
        PrimeField::get_field_with_primitive_root_of_unity(16, 100, &mut ret);
        let (field, primitive_root_of_unity) = ret.clone().unwrap();
        println!(
            "Field: q = {}, root of unity = {}",
            field.q, primitive_root_of_unity
        );
        let domain = field.get_power_series(primitive_root_of_unity);
        // coefficients: vec![6, 2, 5] => P(x) = 5x^2 + 2x + 6
        let mut y_values = domain
            .iter()
            .map(|&x| ((6 + x * (2 + 5 * x)) % field.q + field.q) % field.q)
            .collect::<Vec<i128>>();

        println!("domain = {:?}", domain);
        println!("y_values = {:?}", y_values);
        let rho = 8;
        let s = 6;
        let mut output = vec![];
        prover(
            &y_values,
            field.q,
            rho,
            s,
            &mut output,
            primitive_root_of_unity,
        );
        println!("\n\n\n\n\n\n\n\n\n\n\n***************** PROOF DONE *****************");
        println!("***************** START VERIFY ***************** \n\n");
        assert!(verify(
            field.q,
            s,
            &output,
            y_values.len(),
            primitive_root_of_unity
        ));

        // Change a single y value such that it no longer corresponds to a polynomil
        // a verify that the test fails
        output = vec![];
        y_values[3] = 100;
        y_values[4] = 100;
        prover(
            &y_values,
            field.q,
            rho,
            s,
            &mut output,
            primitive_root_of_unity,
        );
        assert!(!verify(
            field.q,
            s,
            &output,
            y_values.len(),
            primitive_root_of_unity
        ));
    }

    #[test]
    fn generate_proof_16_alt() {
        let mut ret: Option<(PrimeField, i128)> = None;
        // should return (field = mod 193; root = 64) for (n = 16, min_value = 113)
        PrimeField::get_field_with_primitive_root_of_unity(16, 113, &mut ret);
        assert_eq!(193i128, ret.clone().unwrap().0.q);
        let (field, primitive_root_of_unity) = ret.clone().unwrap();
        println!("primitive_root_of_unity = {}", primitive_root_of_unity);
        let domain = field.get_power_series(primitive_root_of_unity);
        assert_eq!(16, domain.len());
        assert_eq!(
            vec![1, 64, 43, 50, 112, 27, 184, 3, 192, 129, 150, 143, 81, 166, 9, 190],
            domain
        );
        let mut output = vec![];

        // corresponds to the polynomial P(x) = x
        // degree < codeword.len() / rho
        let rho = 4;
        let s = 2;
        let y_values = domain;
        prover(
            &y_values,
            field.q,
            rho,
            s,
            &mut output,
            primitive_root_of_unity,
        );
        assert!(verify(
            field.q,
            s,
            &output,
            y_values.len(),
            primitive_root_of_unity
        ));
    }

    #[test]
    fn generate_proof_1024() {
        let mut ret: Option<(PrimeField, i128)> = None;
        PrimeField::get_field_with_primitive_root_of_unity(2i128.pow(14), 2i128.pow(14), &mut ret);
        let (field, primitive_root_of_unity) = ret.clone().unwrap();
        println!(
            "primitive_root_of_unity = {}, prime = {}",
            primitive_root_of_unity, field.q
        );
        assert_eq!(65537i128, field.q);
        assert_eq!(81i128, primitive_root_of_unity);
        let domain = field.get_power_series(primitive_root_of_unity);
        assert_eq!(2usize.pow(14), domain.len());
        // coefficients: vec![6, 2, 5] => P(x) = 5x^2 + 2x + 6
        let mut y_values = domain
            .iter()
            .map(|&x| ((6 + x * (2 + 5 * x)) % field.q + field.q) % field.q)
            .collect::<Vec<i128>>();
    }
}
