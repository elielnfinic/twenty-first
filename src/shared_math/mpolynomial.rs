use crate::shared_math::polynomial::Polynomial;
use crate::shared_math::traits::{IdentityValues, ModPowU64};
use itertools::Itertools;
use num_bigint::BigInt;
use num_traits::Zero;
use std::cmp;
use std::collections::HashMap;
use std::fmt::{Debug, Display};
use std::hash::Hash;
use std::ops::{Add, AddAssign, Div, Mul, Neg, Rem, Sub};

type MCoefficients<T> = HashMap<Vec<u64>, T>;

#[derive(Debug, Clone)]
pub struct MPolynomial<
    T: Add + Div + Mul + Rem + Sub + IdentityValues + Clone + PartialEq + Eq + Hash + Display + Debug,
> {
    // Multivariate polynomials are represented as hash maps with exponent vectors
    // as keys and coefficients as values. E.g.:
    // f(x,y,z) = 17 + 2xy + 42z - 19x^6*y^3*z^12 is represented as:
    // {
    //     [0,0,0] => 17,
    //     [1,1,0] => 2,
    //     [0,0,1] => 42,
    //     [6,3,12] => -19,
    // }
    pub variable_count: usize,
    pub coefficients: HashMap<Vec<u64>, T>,
}

impl<
        U: Add<Output = U>
            + Div<Output = U>
            + Mul<Output = U>
            + Rem
            + Sub<Output = U>
            + Neg<Output = U>
            + ModPowU64
            + IdentityValues
            + Clone
            + PartialEq
            + Eq
            + Hash
            + Display
            + Debug,
    > Display for MPolynomial<U>
{
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        let output;
        if self.is_zero() {
            output = "0".to_string();
        } else {
            let mut term_strings = self
                .coefficients
                .iter()
                .sorted_by_key(|x| x.0[0])
                .map(|(k, v)| Self::term_print(k, v));
            output = term_strings.join("\n+ ");
        }

        write!(f, "\n  {}", output)
    }
}

impl<
        U: Add<Output = U>
            + Div<Output = U>
            + Mul<Output = U>
            + Rem
            + Sub<Output = U>
            + IdentityValues
            + Clone
            + PartialEq
            + Eq
            + Hash
            + Display
            + Debug,
    > PartialEq for MPolynomial<U>
{
    fn eq(&self, other: &Self) -> bool {
        let (shortest, var_count, longest) = if self.variable_count > other.variable_count {
            (
                other.coefficients.clone(),
                self.variable_count,
                self.coefficients.clone(),
            )
        } else {
            (
                self.coefficients.clone(),
                other.variable_count,
                other.coefficients.clone(),
            )
        };

        let mut padded: HashMap<Vec<u64>, U> = HashMap::new();
        for (k, v) in shortest.iter() {
            let mut pad = k.clone();
            pad.resize_with(var_count, || 0);
            padded.insert(pad, v.clone());
        }

        for (fst, snd) in [(padded.clone(), longest.clone()), (longest, padded)] {
            for (k, v) in fst.iter() {
                if !v.is_zero() {
                    if !snd.contains_key(k) {
                        return false;
                    }
                    if snd[k] != *v {
                        return false;
                    }
                }
            }
        }

        true
    }
}

impl<
        U: Add<Output = U>
            + Div<Output = U>
            + Mul<Output = U>
            + Rem
            + Sub<Output = U>
            + IdentityValues
            + Clone
            + PartialEq
            + Eq
            + Hash
            + Display
            + Debug,
    > Eq for MPolynomial<U>
{
}

impl<
        U: Add<Output = U>
            + Div<Output = U>
            + Mul<Output = U>
            + Rem
            + Sub<Output = U>
            + Neg<Output = U>
            + IdentityValues
            + ModPowU64
            + Clone
            + Display
            + Debug
            + PartialEq
            + Eq
            + Hash,
    > MPolynomial<U>
{
    fn term_print(exponents: &[u64], coefficient: &U) -> String {
        if coefficient.is_zero() {
            return "".to_string();
        }

        let mut str_elems: Vec<String> = vec![];
        if !coefficient.is_one() {
            str_elems.push(coefficient.to_string());
        }

        for (i, exponent) in exponents.iter().enumerate() {
            if *exponent == 0 {
                continue;
            }
            let factor_str = if *exponent == 1 {
                format!("x_{}", i)
            } else {
                format!("x_{}^{}", i, exponent)
            };
            str_elems.push(factor_str);
        }

        str_elems.join("*")
    }

    pub fn zero(variable_count: usize) -> Self {
        Self {
            coefficients: HashMap::new(),
            variable_count,
        }
    }

    pub fn is_zero(&self) -> bool {
        if self.coefficients.is_empty() {
            return true;
        }

        for (_, v) in self.coefficients.iter() {
            if !v.is_zero() {
                return false;
            }
        }

        true
    }

    pub fn from_constant(element: U, variable_count: usize) -> Self {
        let mut cs: MCoefficients<U> = HashMap::new();
        cs.insert(vec![0; variable_count], element);
        Self {
            variable_count,
            coefficients: cs,
        }
    }

    // Returns the multivariate polynomials representing each indeterminates linear function
    // with a leading coefficient of one. For three indeterminates, returns:
    // [f(x,y,z) = x, f(x,y,z) = y, f(x,y,z) = z]
    pub fn variables(variable_count: usize, one: U) -> Vec<Self> {
        assert!(one.is_one(), "Provided one must be one");
        let mut res: Vec<Self> = vec![];
        for i in 0..variable_count {
            let mut exponent = vec![0u64; variable_count];
            exponent[i] = 1;
            let mut coefficients: MCoefficients<U> = HashMap::new();
            coefficients.insert(exponent, one.clone());
            res.push(Self {
                variable_count,
                coefficients,
            });
        }

        res
    }

    pub fn evaluate(&self, point: &[U]) -> U {
        assert_eq!(
            self.variable_count,
            point.len(),
            "Dimensionality of multivariate polynomial and point must agree in evaluate"
        );
        let mut acc = point[0].ring_zero();
        for (k, v) in self.coefficients.iter() {
            let mut prod = v.clone();
            for i in 0..k.len() {
                prod = prod.clone() * point[i].mod_pow_u64(k[i]);
            }
            acc = acc + prod;
        }

        acc
    }

    // Substitute the variables in a multivariate polynomial with univariate polynomials, fast
    #[allow(clippy::map_entry)]
    #[allow(clippy::type_complexity)]
    pub fn evaluate_symbolic_with_memoization(
        &self,
        point: &[Polynomial<U>],
        mod_pow_memoization: &mut HashMap<(usize, u64), Polynomial<U>>,
        mul_memoization: &mut HashMap<(Polynomial<U>, (usize, u64)), Polynomial<U>>,
        exponents_memoization: &mut HashMap<Vec<u64>, Polynomial<U>>,
    ) -> Polynomial<U> {
        // Notice that the `exponents_memoization` only gives a speedup if this function is evaluated multiple
        // times for the same `point` input. This condition holds when evaluating the AIR constraints
        // symbolically in a generic STARK prover.
        assert_eq!(
            self.variable_count,
            point.len(),
            "Dimensionality of multivariate polynomial and point must agree in evaluate_symbolic"
        );
        let points_are_x: Vec<bool> = point.iter().map(|p| p.is_x()).collect();
        let mut acc: Polynomial<U> = Polynomial::ring_zero();
        for (k, v) in self.coefficients.iter() {
            let mut prod: Polynomial<U>;
            if exponents_memoization.contains_key(k) {
                prod = exponents_memoization[k].clone();
            } else {
                prod = Polynomial::from_constant(v.ring_one());
                let mut k_sorted: Vec<(usize, u64)> = k.clone().into_iter().enumerate().collect();
                k_sorted.sort_by_key(|k| k.1);
                let mut x_pow_mul = 0;
                for (i, ki) in k_sorted.into_iter() {
                    // calculate prod * point[i].mod_pow(k[i].into(), v.ring_one()) with some optimizations,
                    // mainly memoization.
                    // prod = prod * point[i].mod_pow(k[i].into(), v.ring_one());

                    if ki == 0 {
                        // This should be the common (branch-predicted) case for the early iterations of the inner loop
                        continue;
                    }

                    // Decrease the number of `mul_memoization` misses by doing all powers of x after this inner loop
                    if points_are_x[i] {
                        x_pow_mul += ki;
                        continue;
                    }

                    // With this `mul_key` to lookup previous multiplications there is no risk that we miss
                    // already done calculations in terms of the commutation of multiplication. Since the `ki`
                    // values are sorted there is no risk that we do double work by first calculating
                    // `point[2].mod_pow(2) * point[4].mod_pow(3)` (1) and then
                    // `point[4].mod_pow(3) * point[2].mod_pow(2)` (2). The sorting of the `ki`s ensure that
                    // the calculation will always be done as (1) and never as (2).
                    let mul_key = (prod.clone(), (i, ki));
                    prod = if mul_memoization.contains_key(&mul_key) {
                        // This should be the common case for the late iterations of the inner loop
                        mul_memoization[&mul_key].clone()
                    } else if ki == 1 {
                        let mul_res = prod.clone() * point[i].clone();
                        mul_memoization.insert(mul_key, mul_res.clone());
                        mul_res
                    } else {
                        // Check if we have already done multiplications with a lower power of `point[i]`
                        // than what we are looking for. If we have, then we use this multiplication
                        // as a starting point to calculation the next.

                        let mut reduced_mul_result: Option<Polynomial<U>> = None;
                        let mut reduced_mul_key = (prod.clone(), (i, ki));
                        for j in 1..ki - 1 {
                            reduced_mul_key.1 .1 = ki - j;
                            if mul_memoization.contains_key(&reduced_mul_key) {
                                reduced_mul_result =
                                    Some(mul_memoization[&reduced_mul_key].clone());
                                break;
                            }
                        }

                        let mod_pow_key = match reduced_mul_result {
                            None => (i, ki),
                            // i = 1, ki = 5, found reduced result for (i=1, ki = 2), need mod_pow_key = (i = 1, ki = 3)
                            Some(_) => (i, ki - reduced_mul_key.1 .1),
                        };
                        let mod_pow = if mod_pow_key.1 == 1 {
                            point[i].clone()
                        } else if mod_pow_memoization.contains_key(&mod_pow_key) {
                            mod_pow_memoization[&mod_pow_key].clone()
                        } else {
                            println!("missed mod_pow_memoization!");
                            let mod_pow_res = point[i].mod_pow(mod_pow_key.1.into(), v.ring_one());
                            mod_pow_memoization.insert(mod_pow_key, mod_pow_res.clone());
                            mod_pow_res
                        };
                        let mul_res = match reduced_mul_result {
                            Some(reduced) => reduced * mod_pow,
                            None => prod.clone() * mod_pow,
                        };
                        mul_memoization.insert(mul_key, mul_res.clone());
                        mul_res
                    }
                }

                prod.shift_coefficients_mut(x_pow_mul as usize, v.ring_zero());
                exponents_memoization.insert(k.to_vec(), prod.clone());
            }
            prod.scalar_mul_mut(v.clone());
            acc += prod;
        }

        acc
    }

    // Substitute the variables in a multivariate polynomial with univariate polynomials
    pub fn evaluate_symbolic(&self, point: &[Polynomial<U>]) -> Polynomial<U> {
        assert_eq!(
            self.variable_count,
            point.len(),
            "Dimensionality of multivariate polynomial and point must agree in evaluate_symbolic"
        );
        let mut acc: Polynomial<U> = Polynomial::ring_zero();
        for (k, v) in self.coefficients.iter() {
            let mut prod = Polynomial::from_constant(v.clone());
            for i in 0..k.len() {
                // calculate prod * point[i].mod_pow(k[i].into(), v.ring_one()) with some small optimizations
                // prod = prod * point[i].mod_pow(k[i].into(), v.ring_one());
                prod = if k[i] == 0 {
                    prod
                } else if point[i].is_x() {
                    prod * point[i].shift_coefficients(k[i] as usize - 1, v.ring_zero())
                } else {
                    prod * point[i].mod_pow(k[i].into(), v.ring_one())
                };
            }
            acc += prod;
        }

        acc
    }

    pub fn lift(
        univariate_polynomial: Polynomial<U>,
        variable_index: usize,
        variable_count: usize,
    ) -> Self {
        assert!(
            variable_count > variable_index,
            "number of variables must be at least one larger than the variable index"
        );
        if univariate_polynomial.is_zero() {
            return Self::zero(variable_count);
        }

        let one = univariate_polynomial.coefficients[0].ring_one();
        let mut coefficients: MCoefficients<U> = HashMap::new();
        let mut key = vec![0u64; variable_count];
        key[variable_index] = 1;
        coefficients.insert(key, one.clone());
        let indeterminate: MPolynomial<U> = Self {
            variable_count,
            coefficients,
        };

        let mut acc = MPolynomial::<U>::zero(variable_count);
        for i in 0..univariate_polynomial.coefficients.len() {
            acc += MPolynomial::from_constant(
                univariate_polynomial.coefficients[i].clone(),
                variable_count,
            ) * indeterminate.mod_pow(i.into(), one.clone());
        }

        acc
    }

    pub fn scalar_mul(&self, factor: U) -> Self {
        if self.is_zero() {
            return Self::zero(self.variable_count);
        }

        let mut output_coefficients: MCoefficients<U> = HashMap::new();
        for (k, v) in self.coefficients.iter() {
            output_coefficients.insert(k.to_vec(), v.clone() * factor.clone());
        }

        Self {
            variable_count: self.variable_count,
            coefficients: output_coefficients,
        }
    }

    pub fn scalar_mul_mut(&mut self, factor: U) {
        if self.is_zero() || factor.is_one() {
            return;
        }

        for (_k, v) in self.coefficients.iter_mut() {
            *v = v.to_owned() * factor.clone();
        }
    }

    pub fn mod_pow(&self, pow: BigInt, one: U) -> Self {
        // Handle special case of 0^0
        if pow.is_zero() {
            let mut coefficients: MCoefficients<U> = HashMap::new();
            coefficients.insert(vec![0; self.variable_count], one);
            return MPolynomial {
                variable_count: self.variable_count,
                coefficients,
            };
        }

        // Handle 0^n for n > 0
        if self.is_zero() {
            return Self::zero(self.variable_count);
        }

        let one = self.coefficients.values().last().unwrap().ring_one();
        let exp = vec![0u64; self.variable_count];
        let mut acc_coefficients_init: MCoefficients<U> = HashMap::new();
        acc_coefficients_init.insert(exp, one);
        let mut acc: MPolynomial<U> = Self {
            variable_count: self.variable_count,
            coefficients: acc_coefficients_init,
        };
        let bit_length: u64 = pow.bits();
        for i in 0..bit_length {
            acc = acc.square();
            let set: bool =
                !(pow.clone() & Into::<BigInt>::into(1u128 << (bit_length - 1 - i))).is_zero();
            if set {
                acc = acc * self.clone();
            }
        }

        acc
    }

    pub fn square(&self) -> Self {
        if self.is_zero() {
            return Self::zero(self.variable_count);
        }

        let mut output_coefficients: MCoefficients<U> = HashMap::new();
        let exponents = self.coefficients.keys().collect::<Vec<&Vec<u64>>>();
        let c0 = self.coefficients.values().next().unwrap();
        let two = c0.ring_one() + c0.ring_one();

        for i in 0..exponents.len() {
            let ki = exponents[i];
            let v0 = self.coefficients[ki].clone();
            let mut new_exponents = Vec::with_capacity(self.variable_count);
            for exponent in ki {
                new_exponents.push(exponent * 2);
            }
            if output_coefficients.contains_key(&new_exponents) {
                output_coefficients.insert(
                    new_exponents.to_vec(),
                    v0.to_owned() * v0.to_owned() + output_coefficients[&new_exponents].clone(),
                );
            } else {
                output_coefficients.insert(new_exponents.to_vec(), v0.to_owned() * v0.to_owned());
            }

            for kj in exponents.iter().skip(i + 1) {
                let mut new_exponents = Vec::with_capacity(self.variable_count);
                for k in 0..self.variable_count {
                    // TODO: Can overflow.
                    let exponent = ki[k] + kj[k];
                    new_exponents.push(exponent);
                }
                let v1 = self.coefficients[*kj].clone();
                if output_coefficients.contains_key(&new_exponents) {
                    output_coefficients.insert(
                        new_exponents.to_vec(),
                        two.clone() * v0.to_owned() * v1.to_owned()
                            + output_coefficients[&new_exponents].clone(),
                    );
                } else {
                    output_coefficients.insert(
                        new_exponents.to_vec(),
                        two.clone() * v0.to_owned() * v1.to_owned(),
                    );
                }
            }
        }

        Self {
            coefficients: output_coefficients,
            variable_count: self.variable_count,
        }
    }

    pub fn degree(&self) -> u64 {
        self.coefficients
            .keys()
            .map(|coefficients| coefficients.iter().sum::<u64>())
            .max()
            .unwrap_or(0) as u64
    }
}

impl<
        U: Add<Output = U>
            + Div<Output = U>
            + Mul<Output = U>
            + Rem
            + Sub<Output = U>
            + Neg<Output = U>
            + IdentityValues
            + ModPowU64
            + Clone
            + PartialEq
            + Eq
            + Hash
            + Display
            + Debug,
    > Add for MPolynomial<U>
{
    type Output = Self;

    fn add(self, other: Self) -> Self {
        let variable_count: usize = cmp::max(self.variable_count, other.variable_count);
        if self.is_zero() && other.is_zero() {
            return Self::zero(variable_count);
        }

        let mut output_coefficients: MCoefficients<U> = HashMap::new();
        for (k, v) in self.coefficients.iter() {
            let mut pad = k.clone();
            pad.resize_with(variable_count, || 0);
            output_coefficients.insert(pad, v.clone());
        }
        for (k, v) in other.coefficients.iter() {
            let mut pad = k.clone();
            pad.resize_with(variable_count, || 0);

            // TODO: This can probably be done smarter
            if output_coefficients.contains_key(&pad) {
                output_coefficients.insert(
                    pad.clone(),
                    v.to_owned() + output_coefficients[&pad].clone(),
                );
            } else {
                output_coefficients.insert(pad.to_vec(), v.to_owned());
            }
        }

        Self {
            coefficients: output_coefficients,
            variable_count,
        }
    }
}

impl<
        U: Add<Output = U>
            + Div<Output = U>
            + Mul<Output = U>
            + Rem
            + Sub<Output = U>
            + Neg<Output = U>
            + IdentityValues
            + ModPowU64
            + Clone
            + PartialEq
            + Eq
            + Hash
            + Display
            + Debug,
    > AddAssign for MPolynomial<U>
{
    fn add_assign(&mut self, rhs: Self) {
        if self.variable_count != rhs.variable_count {
            let result = self.clone() + rhs;
            self.variable_count = result.variable_count;
            self.coefficients = result.coefficients;
            return;
        }

        for (k, v1) in rhs.coefficients.iter() {
            if self.coefficients.contains_key(k) {
                let v0 = self.coefficients[k].clone();
                self.coefficients.insert(k.clone(), v0 + v1.to_owned());
            } else {
                self.coefficients.insert(k.clone(), v1.to_owned());
            }
        }
    }
}

impl<
        U: Add<Output = U>
            + Div<Output = U>
            + Mul<Output = U>
            + Rem
            + Sub<Output = U>
            + Neg<Output = U>
            + IdentityValues
            + ModPowU64
            + Clone
            + PartialEq
            + Eq
            + Hash
            + Display
            + Debug,
    > Sub for MPolynomial<U>
{
    type Output = Self;

    fn sub(self, other: Self) -> Self {
        let variable_count: usize = cmp::max(self.variable_count, other.variable_count);
        if self.is_zero() && other.is_zero() {
            return Self::zero(variable_count);
        }

        let mut output_coefficients: MCoefficients<U> = HashMap::new();
        for (k, v) in self.coefficients.iter() {
            let mut pad = k.clone();
            pad.resize_with(variable_count, || 0);
            output_coefficients.insert(pad, v.clone());
        }
        for (k, v) in other.coefficients.iter() {
            let mut pad = k.clone();
            pad.resize_with(variable_count, || 0);

            // TODO: This can probably be done smarter
            if output_coefficients.contains_key(&pad) {
                output_coefficients.insert(
                    pad.to_vec(),
                    output_coefficients[&pad].clone() - v.to_owned(),
                );
            } else {
                output_coefficients.insert(pad.to_vec(), -v.to_owned());
            }
        }

        Self {
            coefficients: output_coefficients,
            variable_count,
        }
    }
}

impl<
        U: Add<Output = U>
            + Div<Output = U>
            + Mul<Output = U>
            + Rem
            + Sub<Output = U>
            + Neg<Output = U>
            + IdentityValues
            + ModPowU64
            + Clone
            + PartialEq
            + Eq
            + Hash
            + Display
            + Debug,
    > Neg for MPolynomial<U>
{
    type Output = Self;

    fn neg(self) -> Self {
        let mut output_coefficients: MCoefficients<U> = HashMap::new();
        for (k, v) in self.coefficients.iter() {
            output_coefficients.insert(k.to_vec(), -v.clone());
        }

        Self {
            variable_count: self.variable_count,
            coefficients: output_coefficients,
        }
    }
}

impl<
        U: Add<Output = U>
            + Div<Output = U>
            + Mul<Output = U>
            + Rem
            + Sub<Output = U>
            + Neg<Output = U>
            + IdentityValues
            + ModPowU64
            + Clone
            + PartialEq
            + Eq
            + Hash
            + Display
            + Debug,
    > Mul for MPolynomial<U>
{
    type Output = Self;

    fn mul(self, other: Self) -> Self {
        let variable_count: usize = cmp::max(self.variable_count, other.variable_count);
        if self.is_zero() || other.is_zero() {
            return Self::zero(variable_count);
        }

        let mut output_coefficients: MCoefficients<U> = HashMap::new();
        for (k0, v0) in self.coefficients.iter() {
            for (k1, v1) in other.coefficients.iter() {
                let mut exponent = vec![0u64; variable_count];
                for k in 0..self.variable_count {
                    exponent[k] += k0[k];
                }
                for k in 0..other.variable_count {
                    exponent[k] += k1[k];
                }
                if output_coefficients.contains_key(&exponent) {
                    output_coefficients.insert(
                        exponent.to_vec(),
                        v0.to_owned() * v1.to_owned() + output_coefficients[&exponent].clone(),
                    );
                } else {
                    output_coefficients.insert(exponent.to_vec(), v0.to_owned() * v1.to_owned());
                }
            }
        }
        Self {
            coefficients: output_coefficients,
            variable_count,
        }
    }
}

#[cfg(test)]
mod test_mpolynomials {
    #![allow(clippy::just_underscores_and_digits)]
    use std::collections::HashSet;

    use crate::shared_math::b_field_element::BFieldElement;
    use crate::utils::generate_random_numbers_u128;

    use super::super::prime_field_element_big::{PrimeFieldBig, PrimeFieldElementBig};
    use super::*;
    use num_bigint::BigInt;
    use rand::RngCore;

    fn b(x: i128) -> BigInt {
        Into::<BigInt>::into(x)
    }

    #[allow(clippy::needless_lifetimes)] // Suppress wrong warning (fails to compile without lifetime, I think)
    fn pfb<'a>(value: i128, field: &'a PrimeFieldBig) -> PrimeFieldElementBig {
        PrimeFieldElementBig::new(b(value), field)
    }

    fn get_x<'a>(field: &'a PrimeFieldBig) -> MPolynomial<PrimeFieldElementBig<'a>> {
        let mut x_coefficients: HashMap<Vec<u64>, PrimeFieldElementBig> = HashMap::new();
        x_coefficients.insert(vec![1, 0, 0], pfb(1, field));
        MPolynomial {
            coefficients: x_coefficients,
            variable_count: 3,
        }
    }

    fn get_x_squared<'a>(field: &'a PrimeFieldBig) -> MPolynomial<PrimeFieldElementBig<'a>> {
        let mut xs_coefficients: HashMap<Vec<u64>, PrimeFieldElementBig> = HashMap::new();
        xs_coefficients.insert(vec![2, 0, 0], pfb(1, field));
        MPolynomial {
            coefficients: xs_coefficients,
            variable_count: 3,
        }
    }

    fn get_x_quartic<'a>(field: &'a PrimeFieldBig) -> MPolynomial<PrimeFieldElementBig<'a>> {
        let mut xs_coefficients: HashMap<Vec<u64>, PrimeFieldElementBig> = HashMap::new();
        xs_coefficients.insert(vec![4, 0, 0], pfb(1, field));
        MPolynomial {
            coefficients: xs_coefficients,
            variable_count: 3,
        }
    }

    fn get_y<'a>(field: &'a PrimeFieldBig) -> MPolynomial<PrimeFieldElementBig<'a>> {
        let mut coefficients: HashMap<Vec<u64>, PrimeFieldElementBig> = HashMap::new();
        coefficients.insert(vec![0, 1, 0], pfb(1, field));
        MPolynomial {
            coefficients,
            variable_count: 3,
        }
    }

    fn get_z<'a>(field: &'a PrimeFieldBig) -> MPolynomial<PrimeFieldElementBig<'a>> {
        let mut z_coefficients: HashMap<Vec<u64>, PrimeFieldElementBig> = HashMap::new();
        z_coefficients.insert(vec![0, 0, 1], pfb(1, field));
        MPolynomial {
            coefficients: z_coefficients,
            variable_count: 3,
        }
    }

    fn get_xz<'a>(field: &'a PrimeFieldBig) -> MPolynomial<PrimeFieldElementBig<'a>> {
        let mut xz_coefficients: HashMap<Vec<u64>, PrimeFieldElementBig> = HashMap::new();
        xz_coefficients.insert(vec![1, 0, 1], pfb(1, field));
        MPolynomial {
            coefficients: xz_coefficients,
            variable_count: 3,
        }
    }

    fn get_x_squared_z_squared<'a>(
        field: &'a PrimeFieldBig,
    ) -> MPolynomial<PrimeFieldElementBig<'a>> {
        let mut xz_coefficients: HashMap<Vec<u64>, PrimeFieldElementBig> = HashMap::new();
        xz_coefficients.insert(vec![2, 0, 2], pfb(1, field));
        MPolynomial {
            coefficients: xz_coefficients,
            variable_count: 3,
        }
    }

    fn get_xyz<'a>(field: &'a PrimeFieldBig) -> MPolynomial<PrimeFieldElementBig<'a>> {
        let mut xyz_coefficients: HashMap<Vec<u64>, PrimeFieldElementBig> = HashMap::new();
        xyz_coefficients.insert(vec![1, 1, 1], pfb(1, field));
        MPolynomial {
            coefficients: xyz_coefficients,
            variable_count: 3,
        }
    }

    fn get_x_plus_xz<'a>(field: &'a PrimeFieldBig) -> MPolynomial<PrimeFieldElementBig<'a>> {
        let mut x_plus_xz_coefficients: HashMap<Vec<u64>, PrimeFieldElementBig> = HashMap::new();
        x_plus_xz_coefficients.insert(vec![1, 0, 1], pfb(1, field));
        x_plus_xz_coefficients.insert(vec![1, 0, 0], pfb(1, field));
        MPolynomial {
            coefficients: x_plus_xz_coefficients,
            variable_count: 3,
        }
    }

    fn get_x_minus_xz<'a>(field: &'a PrimeFieldBig) -> MPolynomial<PrimeFieldElementBig<'a>> {
        let mut x_minus_xz_coefficients: HashMap<Vec<u64>, PrimeFieldElementBig> = HashMap::new();
        x_minus_xz_coefficients.insert(vec![1, 0, 1], pfb(-1, field));
        x_minus_xz_coefficients.insert(vec![1, 0, 0], pfb(1, field));
        MPolynomial {
            coefficients: x_minus_xz_coefficients,
            variable_count: 3,
        }
    }

    fn get_minus_17y<'a>(field: &'a PrimeFieldBig) -> MPolynomial<PrimeFieldElementBig<'a>> {
        let mut _17y_coefficients: HashMap<Vec<u64>, PrimeFieldElementBig> = HashMap::new();
        _17y_coefficients.insert(vec![0, 1, 0], pfb(-17, field));
        MPolynomial {
            coefficients: _17y_coefficients,
            variable_count: 3,
        }
    }

    fn get_x_plus_xz_minus_17y<'a>(
        field: &'a PrimeFieldBig,
    ) -> MPolynomial<PrimeFieldElementBig<'a>> {
        let mut x_plus_xz_minus_17_y_coefficients: HashMap<Vec<u64>, PrimeFieldElementBig> =
            HashMap::new();
        x_plus_xz_minus_17_y_coefficients.insert(vec![1, 0, 1], pfb(1, field));
        x_plus_xz_minus_17_y_coefficients.insert(vec![1, 0, 0], pfb(1, field));
        x_plus_xz_minus_17_y_coefficients.insert(vec![0, 1, 0], pfb(9, field));
        MPolynomial {
            coefficients: x_plus_xz_minus_17_y_coefficients,
            variable_count: 3,
        }
    }

    fn get_big_mpol<'a>(field: &'a PrimeFieldBig) -> MPolynomial<PrimeFieldElementBig<'a>> {
        let mut big_c: HashMap<Vec<u64>, PrimeFieldElementBig> = HashMap::new();
        big_c.insert(vec![0, 0, 1, 0, 0], pfb(1, field));
        big_c.insert(vec![0, 1, 0, 0, 0], pfb(1, field));
        big_c.insert(vec![10, 3, 8, 0, 3], pfb(-9, field));
        big_c.insert(vec![2, 3, 4, 0, 0], pfb(12, field));
        big_c.insert(vec![5, 5, 5, 0, 8], pfb(-4, field));
        big_c.insert(vec![0, 6, 0, 0, 1], pfb(3, field));
        big_c.insert(vec![1, 4, 11, 0, 0], pfb(10, field));
        big_c.insert(vec![1, 0, 12, 0, 2], pfb(2, field));
        MPolynomial {
            coefficients: big_c,
            variable_count: 5,
        }
    }

    fn get_big_mpol_extra_variabel<'a>(
        field: &'a PrimeFieldBig,
    ) -> MPolynomial<PrimeFieldElementBig<'a>> {
        let mut big_c: HashMap<Vec<u64>, PrimeFieldElementBig> = HashMap::new();
        big_c.insert(vec![0, 0, 1, 0, 0, 0], pfb(1, field));
        big_c.insert(vec![0, 1, 0, 0, 0, 0], pfb(1, field));
        big_c.insert(vec![10, 3, 8, 0, 3, 0], pfb(-9, field));
        big_c.insert(vec![2, 3, 4, 0, 0, 0], pfb(12, field));
        big_c.insert(vec![5, 5, 5, 0, 8, 0], pfb(-4, field));
        big_c.insert(vec![0, 6, 0, 0, 1, 0], pfb(3, field));
        big_c.insert(vec![1, 4, 11, 0, 0, 0], pfb(10, field));
        big_c.insert(vec![1, 0, 12, 0, 2, 0], pfb(2, field));
        MPolynomial {
            coefficients: big_c,
            variable_count: 6,
        }
    }

    #[test]
    fn equality_test() {
        let _13 = PrimeFieldBig::new(b(13));
        assert_eq!(get_big_mpol(&_13), get_big_mpol_extra_variabel(&_13));
        assert_ne!(
            get_big_mpol(&_13),
            get_big_mpol_extra_variabel(&_13) + get_x(&&_13)
        );
    }

    #[test]
    fn simple_add_test() {
        let _13 = PrimeFieldBig::new(b(13));
        let x = get_x(&_13);
        let xz = get_xz(&_13);
        let x_plus_xz = get_x_plus_xz(&_13);
        assert_eq!(x_plus_xz, x.clone() + xz.clone());

        let minus_17y = get_minus_17y(&_13);
        let x_plus_xz_minus_17_y = get_x_plus_xz_minus_17y(&_13);
        assert_eq!(x_plus_xz_minus_17_y, x + xz + minus_17y);
    }

    #[test]
    fn simple_sub_test() {
        let _13 = PrimeFieldBig::new(b(13));
        let x = get_x(&_13);
        let xz = get_xz(&_13);
        let x_minus_xz = get_x_minus_xz(&_13);
        assert_eq!(x_minus_xz, x.clone() - xz.clone());

        let big = get_big_mpol(&_13);
        assert_eq!(big.clone(), big.clone() - x.clone() + x.clone());
        assert_eq!(big.clone(), big.clone() - xz.clone() + xz.clone());
        assert_eq!(big.clone(), big.clone() - big.clone() + big.clone());
        assert_eq!(
            big.clone(),
            big.clone() - x_minus_xz.clone() + x_minus_xz.clone()
        );

        // Catch error fixed in sub where similar exponents in both terms of
        // `a(x,y) - b(x,y)` were calculated as `c_b - c_a` instead of as `c_a - c_b`,
        // as it should be.
        let _0 = MPolynomial::from_constant(PrimeFieldElementBig::new(0.into(), &_13), 3);
        let _2 = MPolynomial::from_constant(PrimeFieldElementBig::new(2.into(), &_13), 3);
        let _3 = MPolynomial::from_constant(PrimeFieldElementBig::new(3.into(), &_13), 3);
        let _4 = MPolynomial::from_constant(PrimeFieldElementBig::new(4.into(), &_13), 3);
        let _6 = MPolynomial::from_constant(PrimeFieldElementBig::new(6.into(), &_13), 3);
        let _8 = MPolynomial::from_constant(PrimeFieldElementBig::new(8.into(), &_13), 3);
        assert_eq!(_0, _2.clone() - _2.clone());
        assert_eq!(_0, _4.clone() - _4.clone());
        assert_eq!(_6, _8.clone() - _2.clone());
        assert_eq!(_4, _6.clone() - _2.clone());
        assert_eq!(_2, _4.clone() - _2.clone());
        assert_eq!(_6, _4.clone() + _2.clone());
        assert_eq!(_3, _8.clone() + _8.clone());
    }

    #[test]
    fn simple_mul_test() {
        let _13 = PrimeFieldBig::new(b(13));
        let x = get_x(&_13);
        let z = get_z(&_13);
        let x_squared = get_x_squared(&_13);
        let xz = get_xz(&_13);
        assert_eq!(x_squared, x.clone() * x.clone());
        assert_eq!(xz, x.clone() * z.clone());
    }

    #[test]
    fn simple_modpow_test() {
        let _13 = PrimeFieldBig::new(b(13));
        let one = _13.ring_one();
        let x = get_x(&_13);
        let x_squared = get_x_squared(&_13);
        let x_quartic = get_x_quartic(&_13);
        assert_eq!(x_squared, x.mod_pow(b(2), one.clone()));
        assert_eq!(x_quartic, x.mod_pow(b(4), one.clone()));
        assert_eq!(x_quartic, x_squared.mod_pow(b(2), one.clone()));
        assert_eq!(
            get_x_squared_z_squared(&_13),
            get_xz(&_13).mod_pow(b(2), one.clone())
        );

        assert_eq!(
            x_squared.scalar_mul(pfb(9, &_13)),
            x.scalar_mul(pfb(3, &_13)).mod_pow(b(2), one.clone())
        );
        assert_eq!(
            x_squared.scalar_mul(pfb(16, &_13)),
            x.scalar_mul(pfb(4, &_13)).mod_pow(b(2), one.clone())
        );
        assert_eq!(
            x_quartic.scalar_mul(pfb(16, &_13)),
            x.scalar_mul(pfb(2, &_13)).mod_pow(b(4), one.clone())
        );
        assert_eq!(x_quartic, x.mod_pow(b(4), one.clone()));
        assert_eq!(x_quartic, x_squared.mod_pow(b(2), one.clone()));
        assert_eq!(
            get_x_squared_z_squared(&_13),
            get_xz(&_13).mod_pow(b(2), one.clone())
        );
        assert_eq!(
            get_x_squared_z_squared(&_13).scalar_mul(pfb(25, &_13)),
            get_xz(&_13)
                .scalar_mul(pfb(5, &_13))
                .mod_pow(b(2), one.clone())
        );
        assert_eq!(
            get_big_mpol(&_13) * get_big_mpol(&_13),
            get_big_mpol(&_13).mod_pow(b(2), one.clone())
        );
        assert_eq!(
            get_big_mpol(&_13).scalar_mul(pfb(25, &_13)) * get_big_mpol(&_13),
            get_big_mpol(&_13)
                .scalar_mul(pfb(5, &_13))
                .mod_pow(b(2), one.clone())
        );
    }

    #[test]
    fn variables_test() {
        let _13 = PrimeFieldBig::new(b(13));
        let one = pfb(1, &_13);
        let vars_1 = MPolynomial::variables(1, one.clone());
        assert_eq!(1usize, vars_1.len());
        assert_eq!(get_x(&_13), vars_1[0]);
        let vars_3 = MPolynomial::variables(3, one);
        assert_eq!(3usize, vars_3.len());
        assert_eq!(get_x(&_13), vars_3[0]);
        assert_eq!(get_y(&_13), vars_3[1]);
        assert_eq!(get_z(&_13), vars_3[2]);
    }

    #[test]
    fn evaluate_symbolic_test() {
        let _13 = PrimeFieldBig::new(b(13));
        let zero = PrimeFieldElementBig::new(0.into(), &_13);
        let one = PrimeFieldElementBig::new(1.into(), &_13);
        let two = PrimeFieldElementBig::new(1.into(), &_13);
        let seven = PrimeFieldElementBig::new(7.into(), &_13);
        let xyz_m = get_xyz(&_13);
        let x: Polynomial<PrimeFieldElementBig> =
            Polynomial::from_constant(one.clone()).shift_coefficients(1, zero.clone());
        let x_cubed: Polynomial<PrimeFieldElementBig> =
            Polynomial::from_constant(one.clone()).shift_coefficients(3, zero.clone());
        assert_eq!(
            x_cubed,
            xyz_m.evaluate_symbolic(&vec![x.clone(), x.clone(), x])
        );

        // More complex
        let univariate_pol_1 = Polynomial {
            coefficients: vec![
                one.clone(),
                seven.clone(),
                one.clone(),
                seven.clone(),
                seven.clone(),
                zero.clone(),
            ],
        };
        let univariate_pol_2 = Polynomial {
            coefficients: vec![
                one.clone(),
                seven.clone(),
                one.clone(),
                seven.clone(),
                zero.clone(),
                seven.clone(),
                seven.clone(),
                one.clone(),
                two.clone(),
            ],
        };
        let pol_m = get_x_plus_xz_minus_17y(&_13);
        let evaluated_pol_u = pol_m.evaluate_symbolic(&vec![
            univariate_pol_1.clone(),
            univariate_pol_1,
            univariate_pol_2,
        ]);

        // Calculated on Wolfram Alpha
        let expected_result = Polynomial {
            coefficients: vec![
                PrimeFieldElementBig::new(11.into(), &_13),
                PrimeFieldElementBig::new(6.into(), &_13),
                PrimeFieldElementBig::new(9.into(), &_13),
                PrimeFieldElementBig::new(7.into(), &_13),
                PrimeFieldElementBig::new(7.into(), &_13),
                PrimeFieldElementBig::new(5.into(), &_13),
                PrimeFieldElementBig::new(8.into(), &_13),
                PrimeFieldElementBig::new(2.into(), &_13),
                PrimeFieldElementBig::new(12.into(), &_13),
                PrimeFieldElementBig::new(2.into(), &_13),
                PrimeFieldElementBig::new(5.into(), &_13),
                PrimeFieldElementBig::new(1.into(), &_13),
                PrimeFieldElementBig::new(7.into(), &_13),
            ],
        };

        assert_eq!(expected_result, evaluated_pol_u)
    }

    #[test]
    fn evaluate_symbolic_with_zeros_test() {
        let _13 = PrimeFieldBig::new(b(13));
        let one = PrimeFieldElementBig::new(1.into(), &_13);
        let zero = PrimeFieldElementBig::new(0.into(), &_13);
        let xm = get_x(&_13);
        let xu: Polynomial<PrimeFieldElementBig> =
            Polynomial::from_constant(one).shift_coefficients(1, zero);
        let zero_upol: Polynomial<PrimeFieldElementBig> = Polynomial::ring_zero();
        assert_eq!(
            xu,
            xm.evaluate_symbolic(&vec![xu.clone(), zero_upol.clone(), zero_upol])
        );
    }

    #[test]
    fn evaluate_test() {
        let _13 = PrimeFieldBig::new(b(13));
        let x = get_x(&_13);
        assert_eq!(
            pfb(12, &_13),
            x.evaluate(&vec![pfb(12, &_13), pfb(0, &_13), pfb(0, &_13)])
        );
        assert_eq!(
            pfb(12, &_13),
            x.evaluate(&vec![pfb(12, &_13), pfb(12, &_13), pfb(12, &_13)])
        );

        let xszs = get_x_squared_z_squared(&_13);
        assert_eq!(
            pfb(1, &_13),
            xszs.evaluate(&vec![pfb(12, &_13), pfb(0, &_13), pfb(1, &_13)])
        );
        assert_eq!(
            pfb(1, &_13),
            xszs.evaluate(&vec![pfb(12, &_13), pfb(12, &_13), pfb(12, &_13)])
        );
        assert_eq!(
            pfb(3, &_13),
            xszs.evaluate(&vec![pfb(6, &_13), pfb(3, &_13), pfb(8, &_13)])
        );
        assert_eq!(
            pfb(9, &_13),
            xszs.evaluate(&vec![pfb(8, &_13), pfb(12, &_13), pfb(2, &_13)])
        );
        assert_eq!(
            pfb(3, &_13),
            xszs.evaluate(&vec![pfb(4, &_13), pfb(8, &_13), pfb(1, &_13)])
        );
        assert_eq!(
            pfb(12, &_13),
            xszs.evaluate(&vec![pfb(4, &_13), pfb(9, &_13), pfb(11, &_13)])
        );
        assert_eq!(
            pfb(4, &_13),
            xszs.evaluate(&vec![pfb(1, &_13), pfb(0, &_13), pfb(11, &_13)])
        );
        assert_eq!(
            pfb(0, &_13),
            xszs.evaluate(&vec![pfb(1, &_13), pfb(11, &_13), pfb(0, &_13)])
        );
        assert_eq!(
            pfb(4, &_13),
            xszs.evaluate(&vec![pfb(11, &_13), pfb(0, &_13), pfb(1, &_13)])
        );
    }

    #[test]
    fn lift_test() {
        let _13 = PrimeFieldBig::new(b(13));
        let xm = get_x(&_13);
        let zm = get_z(&_13);
        let xs = Polynomial {
            coefficients: vec![pfb(0, &_13), pfb(1, &_13)],
        };
        assert_eq!(xm, MPolynomial::lift(xs.clone(), 0, 3));
        assert_eq!(zm, MPolynomial::lift(xs.clone(), 2, 3));

        let seven_s = Polynomial {
            coefficients: vec![pfb(7, &_13)],
        };
        assert_eq!(
            MPolynomial::from_constant(pfb(7, &_13), 3),
            MPolynomial::lift(seven_s.clone(), 0, 3)
        );
        assert_ne!(
            MPolynomial::from_constant(pfb(8, &_13), 3),
            MPolynomial::lift(seven_s, 0, 3)
        );

        let x_quartic_s = Polynomial {
            coefficients: vec![
                pfb(0, &_13),
                pfb(0, &_13),
                pfb(0, &_13),
                pfb(0, &_13),
                pfb(1, &_13),
            ],
        };
        assert_eq!(
            get_x_quartic(&_13),
            MPolynomial::lift(x_quartic_s.clone(), 0, 3)
        );
        assert_eq!(
            get_x_quartic(&_13).scalar_mul(pfb(5, &_13)),
            MPolynomial::lift(x_quartic_s.scalar_mul(pfb(5, &_13)).clone(), 0, 3)
        );

        let x_squared_s = Polynomial {
            coefficients: vec![pfb(0, &_13), pfb(0, &_13), pfb(1, &_13)],
        };
        assert_eq!(
            get_x_quartic(&_13) + get_x_squared(&_13) + get_x(&_13),
            MPolynomial::lift(x_quartic_s.clone() + x_squared_s.clone() + xs.clone(), 0, 3)
        );
        assert_eq!(
            get_x_quartic(&_13).scalar_mul(pfb(5, &_13))
                + get_x_squared(&_13).scalar_mul(pfb(4, &_13))
                + get_x(&_13).scalar_mul(pfb(3, &_13)),
            MPolynomial::lift(
                x_quartic_s.scalar_mul(pfb(5, &_13))
                    + x_squared_s.scalar_mul(pfb(4, &_13))
                    + xs.scalar_mul(pfb(3, &_13)),
                0,
                3
            )
        );
    }

    #[test]
    fn add_assign_simple_test() {
        for i in 0..10 {
            let mut a = gen_mpolynomial(i, i, 14, u64::MAX);
            let a_clone = a.clone();
            let mut b = gen_mpolynomial(i, i, 140, u64::MAX);
            let b_clone = b.clone();
            a += b_clone.clone();
            assert_eq!(a_clone.clone() + b_clone.clone(), a);
            b += a_clone.clone();
            assert_eq!(a_clone + b_clone, b);
        }
    }

    #[test]
    fn square_test_simple() {
        let _13 = PrimeFieldBig::new(b(13));
        let xz = get_xz(&_13);
        let xz_squared = get_x_squared_z_squared(&_13);
        assert_eq!(xz_squared, xz.square());
    }

    #[test]
    fn square_test() {
        for i in 0..10 {
            let poly = gen_mpolynomial(i, i, 7, u64::MAX);
            let actual = poly.square();
            let expected = poly.clone() * poly;
            assert_eq!(expected, actual);
        }
    }

    #[test]
    fn mul_commutative_test() {
        let a = gen_mpolynomial(40, 40, 100, u64::MAX);
        let b = gen_mpolynomial(20, 20, 1000, u64::MAX);
        let ab = a.clone() * b.clone();
        let ba = b.clone() * a.clone();
        assert_eq!(ab, ba);
    }

    #[test]
    fn mod_pow_test() {
        let a = gen_mpolynomial(4, 6, 2, 20);
        let mut acc = MPolynomial::from_constant(BFieldElement::ring_one(), 4);
        for i in 0..10 {
            let mod_pow = a.mod_pow(i.into(), BFieldElement::ring_one());
            println!(
                "mod_pow.coefficients.len() = {}",
                mod_pow.coefficients.len()
            );
            assert!(unique_exponent_vectors(&mod_pow));
            assert_eq!(acc, mod_pow);
            acc = acc.clone() * a.clone();
        }
    }

    fn unique_exponent_vectors(input: &MPolynomial<BFieldElement>) -> bool {
        let mut hashset: HashSet<Vec<u64>> = HashSet::new();

        input
            .coefficients
            .iter()
            .all(|(k, _v)| hashset.insert(k.clone()))
    }

    fn gen_mpolynomial(
        variable_count: usize,
        term_count: usize,
        exponenent_limit: u128,
        coefficient_limit: u64,
    ) -> MPolynomial<BFieldElement> {
        let mut coefficients: HashMap<Vec<u64>, BFieldElement> = HashMap::new();

        for _ in 0..term_count {
            let key = generate_random_numbers_u128(variable_count, None)
                .iter()
                .map(|x| (*x % exponenent_limit) as u64)
                .collect::<Vec<u64>>();
            let value = gen_bfield_element(coefficient_limit);
            coefficients.insert(key, value);
        }

        MPolynomial {
            variable_count,
            coefficients,
        }
    }

    fn gen_bfield_element(limit: u64) -> BFieldElement {
        let mut rng = rand::thread_rng();
        let elem = rng.next_u64() % limit;
        BFieldElement::new(elem as u128)
    }
}
