use std::collections::HashMap;
use std::fmt::Debug;
use std::fmt::Display;
use std::fmt::Formatter;
use std::hash::Hash;
use std::ops::Add;
use std::ops::AddAssign;
use std::ops::Div;
use std::ops::Mul;
use std::ops::MulAssign;
use std::ops::Neg;
use std::ops::Rem;
use std::ops::Sub;
use std::thread::available_parallelism;

use arbitrary::Arbitrary;
use itertools::EitherOrBoth;
use itertools::Itertools;
use num_bigint::BigInt;
use num_traits::One;
use num_traits::Zero;
use rayon::prelude::*;

use crate::math::ntt::intt;
use crate::math::ntt::ntt;
use crate::math::traits::FiniteField;
use crate::math::traits::ModPowU32;
use crate::prelude::BFieldElement;
use crate::prelude::Inverse;
use crate::prelude::XFieldElement;

use super::traits::PrimitiveRootOfUnity;

impl<FF: FiniteField> Zero for Polynomial<FF> {
    fn zero() -> Self {
        Self {
            coefficients: vec![],
        }
    }

    fn is_zero(&self) -> bool {
        *self == Self::zero()
    }
}

impl<FF: FiniteField> One for Polynomial<FF> {
    fn one() -> Self {
        Self {
            coefficients: vec![FF::one()],
        }
    }

    fn is_one(&self) -> bool {
        self.degree() == 0 && self.coefficients[0].is_one()
    }
}

/// A univariate polynomial with coefficients in a [finite field](FiniteField), in monomial form.
#[derive(Clone, Arbitrary)]
pub struct Polynomial<FF: FiniteField> {
    /// The polynomial's coefficients, in order of increasing degree. That is, the polynomial's
    /// leading coefficient is the last element of the vector.
    pub coefficients: Vec<FF>,
}

impl<FF: FiniteField> Debug for Polynomial<FF> {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Polynomial")
            .field("coefficients", &self.coefficients)
            .finish()
    }
}

// Not derived because `PartialEq` is also not derived.
impl<FF: FiniteField> Hash for Polynomial<FF> {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.coefficients.hash(state);
    }
}

impl<FF: FiniteField> Display for Polynomial<FF> {
    fn fmt(&self, f: &mut Formatter) -> std::fmt::Result {
        let degree = match self.degree() {
            -1 => return write!(f, "0"),
            d => d as usize,
        };

        for pow in (0..=degree).rev() {
            let coeff = self.coefficients[pow];
            if coeff.is_zero() {
                continue;
            }

            if pow != degree {
                write!(f, " + ")?;
            }
            if !coeff.is_one() || pow == 0 {
                write!(f, "{coeff}")?;
            }
            match pow {
                0 => (),
                1 => write!(f, "x")?,
                _ => write!(f, "x^{pow}")?,
            }
        }

        Ok(())
    }
}

// Manually implemented to correctly handle leading zeros.
impl<FF: FiniteField> PartialEq for Polynomial<FF> {
    fn eq(&self, other: &Self) -> bool {
        if self.degree() != other.degree() {
            return false;
        }

        self.coefficients
            .iter()
            .zip(other.coefficients.iter())
            .all(|(x, y)| x == y)
    }
}

impl<FF: FiniteField> Eq for Polynomial<FF> {}

impl<FF> Polynomial<FF>
where
    FF: FiniteField + MulAssign<BFieldElement>,
{
    /// [Fast multiplication](Self::multiply) is slower than [naïve multiplication](Self::mul)
    /// for polynomials of degree less than this threshold.
    ///
    /// Extracted from `cargo bench --bench poly_mul` on mjolnir.
    const FAST_MULTIPLY_CUTOFF_THRESHOLD: isize = 1 << 8;

    /// [Divide and conquer batch evaluation](Self::divide_and_conquer_batch_evaluate)
    /// is slower than [iterative evaluation](Self::iterative_batch_evaluate)
    /// on domains smaller than this threshold
    const DIVIDE_AND_CONQUER_EVALUATE_CUTOFF_THRESHOLD: usize = 1 << 5;

    /// Computing the [fast zerofier][fast] is slower than computing the [smart zerofier][smart] for
    /// domain sizes smaller than this threshold. The [naïve zerofier][naive] is always slower to
    /// compute than the [smart zerofier][smart] for domain sizes smaller than the threshold.
    ///
    /// Extracted from `cargo bench --bench zerofier` on mjolnir.
    ///
    /// [naive]: Self::naive_zerofier
    /// [smart]: Self::smart_zerofier
    /// [fast]: Self::fast_zerofier
    const FAST_ZEROFIER_CUTOFF_THRESHOLD: usize = 200;

    /// [Fast interpolation](Self::fast_interpolate) is slower than
    /// [Lagrange interpolation](Self::lagrange_interpolate) below this threshold.
    ///
    /// Extracted from `cargo bench --bench interpolation` on mjolnir.
    const FAST_INTERPOLATE_CUTOFF_THRESHOLD: usize = 1 << 8;

    /// Inside `formal_power_series_inverse`, when to multiply naively and when
    /// to use NTT-based multiplication. Use benchmark
    /// `formal_power_series_inverse` to find the optimum. Based on benchmarks,
    /// the optimum probably lies somewhere between 2^5 and 2^9.
    const FORMAL_POWER_SERIES_INVERSE_CUTOFF: usize = 1 << 8;

    /// Modular reduction is made fast by first finding a multiple of the
    /// denominator that allows for chunk-wise reduction, and then finishing off
    /// by reducing by the plain denominator using plain long division. The
    /// "fast"ness comes from using NTT-based multiplication in the chunk-wise
    /// reduction step. This const regulates the chunk size and thus the domain
    /// size of the NTT.
    const FAST_REDUCE_CUTOFF_THRESHOLD: usize = 1 << 8;

    /// When doing batch evaluation, sometimes it makes sense to reduce the
    /// polynomial modulo the zerofier of the domain first. This const regulates
    /// when.
    const REDUCE_BEFORE_EVALUATE_THRESHOLD_RATIO: isize = 4;

    /// Return the polynomial which corresponds to the transformation `x → α·x`.
    ///
    /// Given a polynomial P(x), produce P'(x) := P(α·x). Evaluating P'(x) then corresponds to
    /// evaluating P(α·x).
    #[must_use]
    pub fn scale<S, XF>(&self, alpha: S) -> Polynomial<XF>
    where
        S: Clone + One,
        FF: Mul<S, Output = XF>,
        XF: FiniteField,
    {
        let mut power_of_alpha = S::one();
        let mut return_coefficients = Vec::with_capacity(self.coefficients.len());
        for &coefficient in &self.coefficients {
            return_coefficients.push(coefficient * power_of_alpha.clone());
            power_of_alpha = power_of_alpha * alpha.clone();
        }
        Polynomial::new(return_coefficients)
    }

    /// It is the caller's responsibility that this function is called with sufficiently large input
    /// to be safe and to be faster than `square`.
    #[must_use]
    pub fn fast_square(&self) -> Self {
        let degree = self.degree();
        if degree == -1 {
            return Self::zero();
        }
        if degree == 0 {
            return Self::from_constant(self.coefficients[0] * self.coefficients[0]);
        }

        let result_degree: u64 = 2 * self.degree() as u64;
        let order = (result_degree + 1).next_power_of_two();
        let root_res = BFieldElement::primitive_root_of_unity(order);
        let root =
            root_res.unwrap_or_else(|| panic!("primitive root for order {order} should exist"));

        let mut coefficients = self.coefficients.to_vec();
        coefficients.resize(order as usize, FF::zero());
        let log_2_of_n = coefficients.len().ilog2();
        ntt::<FF>(&mut coefficients, root, log_2_of_n);

        for element in coefficients.iter_mut() {
            *element = element.to_owned() * element.to_owned();
        }

        intt::<FF>(&mut coefficients, root, log_2_of_n);
        coefficients.truncate(result_degree as usize + 1);

        Polynomial { coefficients }
    }

    #[must_use]
    pub fn square(&self) -> Self {
        let degree = self.degree();
        if degree == -1 {
            return Self::zero();
        }

        // A benchmark run on sword_smith's PC revealed that `fast_square` was faster when the input
        // size exceeds a length of 64.
        let squared_coefficient_len = self.degree() as usize * 2 + 1;
        if squared_coefficient_len > 64 {
            return self.fast_square();
        }

        let zero = FF::zero();
        let one = FF::one();
        let two = one + one;
        let mut squared_coefficients = vec![zero; squared_coefficient_len];

        // TODO: Review.
        for i in 0..self.coefficients.len() {
            let ci = self.coefficients[i];
            squared_coefficients[2 * i] += ci * ci;

            for j in i + 1..self.coefficients.len() {
                let cj = self.coefficients[j];
                squared_coefficients[i + j] += two * ci * cj;
            }
        }

        Self {
            coefficients: squared_coefficients,
        }
    }

    #[must_use]
    pub fn fast_mod_pow(&self, pow: BigInt) -> Self {
        let one = FF::one();

        // Special case to handle 0^0 = 1
        if pow.is_zero() {
            return Self::from_constant(one);
        }

        if self.is_zero() {
            return Self::zero();
        }

        if pow.is_one() {
            return self.clone();
        }

        let mut acc = Polynomial::from_constant(one);
        let bit_length: u64 = pow.bits();
        for i in 0..bit_length {
            acc = acc.square();
            let set: bool =
                !(pow.clone() & Into::<BigInt>::into(1u128 << (bit_length - 1 - i))).is_zero();
            if set {
                acc = self.to_owned() * acc;
            }
        }

        acc
    }

    /// Multiply `self` by `other`.
    ///
    /// Prefer this over [`self * other`](Self::mul) since it chooses the fastest multiplication
    /// strategy.
    #[must_use]
    pub fn multiply(&self, other: &Self) -> Self {
        if self.degree() + other.degree() < Self::FAST_MULTIPLY_CUTOFF_THRESHOLD {
            self.naive_multiply(other)
        } else {
            self.fast_multiply(other)
        }
    }

    /// Use [Self::multiply] instead. Only `pub` to allow benchmarking; not considered part of the
    /// public API.
    ///
    /// This method is asymptotically faster than [naive multiplication](Self::naive_multiply). For
    /// small instances, _i.e._, polynomials of low degree, it is slower.
    ///
    /// The time complexity of this method is in O(n·log(n)), where `n` is the sum of the degrees
    /// of the operands. The time complexity of the naive multiplication is in O(n^2).
    #[doc(hidden)]
    pub fn fast_multiply(&self, other: &Self) -> Self {
        let Ok(degree) = usize::try_from(self.degree() + other.degree()) else {
            return Self::zero();
        };
        let order = (degree + 1).next_power_of_two();
        let order_u64 = u64::try_from(order).unwrap();
        let root = BFieldElement::primitive_root_of_unity(order_u64).unwrap();

        let mut lhs_coefficients = self.coefficients.to_vec();
        let mut rhs_coefficients = other.coefficients.to_vec();

        lhs_coefficients.resize(order, FF::zero());
        rhs_coefficients.resize(order, FF::zero());

        ntt::<FF>(&mut lhs_coefficients, root, order.ilog2());
        ntt::<FF>(&mut rhs_coefficients, root, order.ilog2());

        let mut hadamard_product: Vec<FF> = rhs_coefficients
            .into_iter()
            .zip(lhs_coefficients)
            .map(|(r, l)| r * l)
            .collect();

        intt::<FF>(&mut hadamard_product, root, order.ilog2());
        hadamard_product.truncate(degree + 1);
        Self::new(hadamard_product)
    }

    /// Compute the lowest degree polynomial with the provided roots.
    /// Also known as “vanishing polynomial.”
    ///
    /// # Example
    ///
    /// ```
    /// # use num_traits::Zero;
    /// # use twenty_first::prelude::*;
    /// let roots = bfe_array![2, 4, 6];
    /// let zerofier = Polynomial::zerofier(&roots);
    ///
    /// assert_eq!(3, zerofier.degree());
    /// assert_eq!(bfe_vec![0, 0, 0], zerofier.batch_evaluate(&roots));
    ///
    /// let  non_roots = bfe_vec![0, 1, 3, 5];
    /// assert!(zerofier.batch_evaluate(&non_roots).iter().all(|x| !x.is_zero()));
    /// ```
    pub fn zerofier(roots: &[FF]) -> Self {
        if roots.len() < Self::FAST_ZEROFIER_CUTOFF_THRESHOLD {
            Self::smart_zerofier(roots)
        } else {
            Self::fast_zerofier(roots)
        }
    }

    /// Only `pub` to allow benchmarking; not considered part of the public API.
    #[doc(hidden)]
    pub fn smart_zerofier(roots: &[FF]) -> Self {
        let mut zerofier = vec![FF::zero(); roots.len() + 1];
        zerofier[0] = FF::one();
        let mut num_coeffs = 1;
        for &root in roots {
            for k in (1..=num_coeffs).rev() {
                zerofier[k] = zerofier[k - 1] - root * zerofier[k];
            }
            zerofier[0] = -root * zerofier[0];
            num_coeffs += 1;
        }
        Self::new(zerofier)
    }

    /// Only `pub` to allow benchmarking; not considered part of the public API.
    #[doc(hidden)]
    pub fn fast_zerofier(roots: &[FF]) -> Self {
        let mid_point = roots.len() / 2;
        let (left, right) = rayon::join(
            || Self::zerofier(&roots[..mid_point]),
            || Self::zerofier(&roots[mid_point..]),
        );

        left.multiply(&right)
    }

    /// Construct the lowest-degree polynomial interpolating the given points.
    ///
    /// ```
    /// # use twenty_first::prelude::*;
    /// let domain = bfe_vec![0, 1, 2, 3];
    /// let values = bfe_vec![1, 3, 5, 7];
    /// let polynomial = Polynomial::interpolate(&domain, &values);
    ///
    /// assert_eq!(1, polynomial.degree());
    /// assert_eq!(bfe!(9), polynomial.evaluate(bfe!(4)));
    /// ```
    ///
    /// # Panics
    ///
    /// - Panics if the provided domain is empty.
    /// - Panics if the provided domain and values are not of the same length.
    pub fn interpolate(domain: &[FF], values: &[FF]) -> Self {
        assert!(
            !domain.is_empty(),
            "interpolation must happen through more than zero points"
        );
        assert_eq!(
            domain.len(),
            values.len(),
            "The domain and values lists have to be of equal length."
        );

        if domain.len() <= Self::FAST_INTERPOLATE_CUTOFF_THRESHOLD {
            Self::lagrange_interpolate(domain, values)
        } else {
            Self::fast_interpolate(domain, values)
        }
    }

    /// Any fast interpolation will use NTT, so this is mainly used for testing/integrity
    /// purposes. This also means that it is not pivotal that this function has an optimal
    /// runtime.
    #[doc(hidden)]
    pub fn lagrange_interpolate_zipped(points: &[(FF, FF)]) -> Self {
        assert!(
            !points.is_empty(),
            "interpolation must happen through more than zero points"
        );
        assert!(
            points.iter().map(|x| x.0).all_unique(),
            "Repeated x values received. Got: {points:?}",
        );

        let xs: Vec<FF> = points.iter().map(|x| x.0.to_owned()).collect();
        let ys: Vec<FF> = points.iter().map(|x| x.1.to_owned()).collect();
        Self::lagrange_interpolate(&xs, &ys)
    }

    #[doc(hidden)]
    pub fn lagrange_interpolate(domain: &[FF], values: &[FF]) -> Self {
        debug_assert!(
            !domain.is_empty(),
            "interpolation domain cannot have zero points"
        );
        debug_assert_eq!(domain.len(), values.len());

        let zero = FF::zero();
        let zerofier = Self::zerofier(domain).coefficients;

        // In each iteration of this loop, accumulate into the sum one polynomial that evaluates
        // to some abscis (y-value) in the given ordinate (domain point), and to zero in all other
        // ordinates.
        let mut lagrange_sum_array = vec![zero; domain.len()];
        let mut summand_array = vec![zero; domain.len()];
        for (i, &abscis) in values.iter().enumerate() {
            // divide (X - domain[i]) out of zerofier to get unweighted summand
            let mut leading_coefficient = zerofier[domain.len()];
            let mut supporting_coefficient = zerofier[domain.len() - 1];
            let mut summand_eval = zero;
            for j in (1..domain.len()).rev() {
                summand_array[j] = leading_coefficient;
                summand_eval = summand_eval * domain[i] + leading_coefficient;
                leading_coefficient = supporting_coefficient + leading_coefficient * domain[i];
                supporting_coefficient = zerofier[j - 1];
            }

            // avoid `j - 1` for j == 0 in the loop above
            summand_array[0] = leading_coefficient;
            summand_eval = summand_eval * domain[i] + leading_coefficient;

            // summand does not necessarily evaluate to 1 in domain[i]: correct for this value
            let corrected_abscis = abscis / summand_eval;

            // accumulate term
            for j in 0..domain.len() {
                lagrange_sum_array[j] += corrected_abscis * summand_array[j];
            }
        }

        Self::new(lagrange_sum_array)
    }

    /// Only `pub` to allow benchmarking; not considered part of the public API.
    #[doc(hidden)]
    pub fn fast_interpolate(domain: &[FF], values: &[FF]) -> Self {
        debug_assert!(
            !domain.is_empty(),
            "interpolation domain cannot have zero points"
        );
        debug_assert_eq!(domain.len(), values.len());

        // prevent edge case failure where the left half would be empty
        if domain.len() == 1 {
            return Self::from_constant(values[0]);
        }

        let mid_point = domain.len() / 2;
        let left_domain_half = &domain[..mid_point];
        let left_values_half = &values[..mid_point];
        let right_domain_half = &domain[mid_point..];
        let right_values_half = &values[mid_point..];

        let (left_zerofier, right_zerofier) = rayon::join(
            || Self::zerofier(left_domain_half),
            || Self::zerofier(right_domain_half),
        );

        let (left_offset, right_offset) = rayon::join(
            || right_zerofier.batch_evaluate(left_domain_half),
            || left_zerofier.batch_evaluate(right_domain_half),
        );

        let hadamard_mul = |x: &[_], y: Vec<_>| x.iter().zip(y).map(|(&n, d)| n * d).collect_vec();
        let interpolate_half = |offset, domain_half, values_half| {
            let offset_inverse = FF::batch_inversion(offset);
            let targets = hadamard_mul(values_half, offset_inverse);
            Self::interpolate(domain_half, &targets)
        };
        let (left_interpolant, right_interpolant) = rayon::join(
            || interpolate_half(left_offset, left_domain_half, left_values_half),
            || interpolate_half(right_offset, right_domain_half, right_values_half),
        );

        let (left_term, right_term) = rayon::join(
            || left_interpolant.multiply(&right_zerofier),
            || right_interpolant.multiply(&left_zerofier),
        );

        left_term + right_term
    }

    pub fn batch_fast_interpolate(
        domain: &[FF],
        values_matrix: &Vec<Vec<FF>>,
        primitive_root: BFieldElement,
        root_order: usize,
    ) -> Vec<Self> {
        debug_assert_eq!(
            primitive_root.mod_pow_u32(root_order as u32),
            BFieldElement::one(),
            "Supplied element “primitive_root” must have supplied order.\
            Supplied element was: {primitive_root:?}\
            Supplied order was: {root_order:?}"
        );

        assert!(
            !domain.is_empty(),
            "Cannot fast interpolate through zero points.",
        );

        let mut zerofier_dictionary: HashMap<(FF, FF), Polynomial<FF>> = HashMap::default();
        let mut offset_inverse_dictionary: HashMap<(FF, FF), Vec<FF>> = HashMap::default();

        Self::batch_fast_interpolate_with_memoization(
            domain,
            values_matrix,
            &mut zerofier_dictionary,
            &mut offset_inverse_dictionary,
        )
    }

    fn batch_fast_interpolate_with_memoization(
        domain: &[FF],
        values_matrix: &Vec<Vec<FF>>,
        zerofier_dictionary: &mut HashMap<(FF, FF), Polynomial<FF>>,
        offset_inverse_dictionary: &mut HashMap<(FF, FF), Vec<FF>>,
    ) -> Vec<Self> {
        // This value of 16 was found to be optimal through a benchmark on sword_smith's
        // machine.
        const OPTIMAL_CUTOFF_POINT_FOR_BATCHED_INTERPOLATION: usize = 16;
        if domain.len() < OPTIMAL_CUTOFF_POINT_FOR_BATCHED_INTERPOLATION {
            return values_matrix
                .iter()
                .map(|values| Self::lagrange_interpolate(domain, values))
                .collect();
        }

        // calculate everything related to the domain
        let half = domain.len() / 2;

        let left_key = (domain[0], domain[half - 1]);
        let left_zerofier = match zerofier_dictionary.get(&left_key) {
            Some(z) => z.to_owned(),
            None => {
                let left_zerofier = Self::zerofier(&domain[..half]);
                zerofier_dictionary.insert(left_key, left_zerofier.clone());
                left_zerofier
            }
        };
        let right_key = (domain[half], *domain.last().unwrap());
        let right_zerofier = match zerofier_dictionary.get(&right_key) {
            Some(z) => z.to_owned(),
            None => {
                let right_zerofier = Self::zerofier(&domain[half..]);
                zerofier_dictionary.insert(right_key, right_zerofier.clone());
                right_zerofier
            }
        };

        let left_offset_inverse = match offset_inverse_dictionary.get(&left_key) {
            Some(vector) => vector.to_owned(),
            None => {
                let left_offset: Vec<FF> =
                    Self::divide_and_conquer_batch_evaluate(&right_zerofier, &domain[..half]);
                let left_offset_inverse = FF::batch_inversion(left_offset);
                offset_inverse_dictionary.insert(left_key, left_offset_inverse.clone());
                left_offset_inverse
            }
        };
        let right_offset_inverse = match offset_inverse_dictionary.get(&right_key) {
            Some(vector) => vector.to_owned(),
            None => {
                let right_offset: Vec<FF> =
                    Self::divide_and_conquer_batch_evaluate(&left_zerofier, &domain[half..]);
                let right_offset_inverse = FF::batch_inversion(right_offset);
                offset_inverse_dictionary.insert(right_key, right_offset_inverse.clone());
                right_offset_inverse
            }
        };

        // prepare target matrices
        let all_left_targets: Vec<_> = values_matrix
            .par_iter()
            .map(|values| {
                values[..half]
                    .iter()
                    .zip(left_offset_inverse.iter())
                    .map(|(n, d)| n.to_owned() * *d)
                    .collect()
            })
            .collect();
        let all_right_targets: Vec<_> = values_matrix
            .par_iter()
            .map(|values| {
                values[half..]
                    .par_iter()
                    .zip(right_offset_inverse.par_iter())
                    .map(|(n, d)| n.to_owned() * *d)
                    .collect()
            })
            .collect();

        // recurse
        let left_interpolants = Self::batch_fast_interpolate_with_memoization(
            &domain[..half],
            &all_left_targets,
            zerofier_dictionary,
            offset_inverse_dictionary,
        );
        let right_interpolants = Self::batch_fast_interpolate_with_memoization(
            &domain[half..],
            &all_right_targets,
            zerofier_dictionary,
            offset_inverse_dictionary,
        );

        // add vectors of polynomials
        let interpolants = left_interpolants
            .par_iter()
            .zip(right_interpolants.par_iter())
            .map(|(left_interpolant, right_interpolant)| {
                let left_term = left_interpolant.multiply(&right_zerofier);
                let right_term = right_interpolant.multiply(&left_zerofier);

                left_term + right_term
            })
            .collect();

        interpolants
    }

    /// Evaluate the polynomial on a batch of points.
    pub fn batch_evaluate(&self, domain: &[FF]) -> Vec<FF> {
        if self.degree() >= Self::REDUCE_BEFORE_EVALUATE_THRESHOLD_RATIO * (domain.len() as isize) {
            self.reduce_then_batch_evaluate(domain)
        } else if domain.len() >= Self::DIVIDE_AND_CONQUER_EVALUATE_CUTOFF_THRESHOLD {
            self.divide_and_conquer_batch_evaluate(domain)
        } else {
            self.iterative_batch_evaluate(domain)
        }
    }

    fn reduce_then_batch_evaluate(&self, domain: &[FF]) -> Vec<FF> {
        let zerofier = Self::zerofier(domain);
        let remainder = self.fast_reduce(&zerofier);
        remainder.batch_evaluate(domain)
    }

    /// Parallel version of [`batch_evaluate`](Self::batch_evalaute).
    pub fn par_batch_evaluate(&self, domain: &[FF]) -> Vec<FF> {
        let num_threads = available_parallelism().map(|nzu| nzu.get()).unwrap_or(1);
        let chunk_size = (domain.len() + num_threads - 1) / num_threads;
        let chunks = domain.chunks(chunk_size).collect_vec();
        chunks
            .into_par_iter()
            .flat_map(|ch| self.batch_evaluate(ch))
            .collect()
    }

    /// Only marked `pub` for benchmarking; not considered part of hte public API.
    #[doc(hidden)]
    pub fn iterative_batch_evaluate(&self, domain: &[FF]) -> Vec<FF> {
        domain.iter().map(|&p| self.evaluate(p)).collect()
    }

    /// Only marked `pub` for benchmarking; not considered part of hte public API.
    #[doc(hidden)]
    pub fn parallel_evaluate(&self, domain: &[FF]) -> Vec<FF> {
        domain.par_iter().map(|&p| self.evaluate(p)).collect()
    }

    /// Only `pub` to allow benchmarking; not considered part of the public API.
    #[doc(hidden)]
    pub fn vector_batch_evaluate(&self, domain: &[FF]) -> Vec<FF> {
        let mut accumulators = vec![FF::zero(); domain.len()];
        for &c in self.coefficients.iter().rev() {
            accumulators
                .par_iter_mut()
                .zip(domain)
                .for_each(|(acc, &x)| *acc = *acc * x + c);
        }

        accumulators
    }

    /// Only `pub` to allow benchmarking; not considered part of the public API.
    #[doc(hidden)]
    pub fn divide_and_conquer_batch_evaluate(&self, domain: &[FF]) -> Vec<FF> {
        let mid_point = domain.len() / 2;
        let left_half = &domain[..mid_point];
        let right_half = &domain[mid_point..];

        [left_half, right_half]
            .into_iter()
            .flat_map(|half_domain| self.batch_evaluate(half_domain))
            .collect()
    }

    /// Fast evaluate on a coset domain, which is the group generated by `generator^i * offset`.
    ///
    /// # Performance
    ///
    /// If possible, use a [base field element](BFieldElement) as the offset.
    ///
    /// # Panics
    ///
    /// Panics if the order of the domain generated by the `generator` is smaller than or equal to
    /// the degree of `self`.
    pub fn fast_coset_evaluate<S>(
        &self,
        offset: S,
        generator: BFieldElement,
        order: usize,
    ) -> Vec<FF>
    where
        S: Clone + One,
        FF: Mul<S, Output = FF>,
    {
        // NTT's input and output are of the same size. For domains of an order that is larger than
        // or equal to the number of coefficients of the polynomial, padding with leading zeros
        // (a no-op to the polynomial) achieves this requirement. However, if the order is smaller
        // than the number of coefficients in the polynomial, this would mean chopping off leading
        // coefficients, which changes the polynomial. Therefore, this method is currently limited
        // to domain orders greater than the degree of the polynomial.
        assert!(
            (order as isize) > self.degree(),
            "`Polynomial::fast_coset_evaluate` is currently limited to domains of order \
            greater than the degree of the polynomial."
        );

        let mut coefficients = self.scale(offset).coefficients;
        coefficients.resize(order, FF::zero());

        let log_2_of_n = coefficients.len().ilog2();
        ntt::<FF>(&mut coefficients, generator, log_2_of_n);
        coefficients
    }

    /// The inverse of [`Self::fast_coset_evaluate`].
    ///
    /// # Performance
    ///
    /// If possible, use a [base field element](BFieldElement) as the offset.
    ///
    /// # Panics
    ///
    /// Panics if the length of `values` does not equal the order of the domain generated by the
    /// `generator`.
    pub fn fast_coset_interpolate<S>(offset: S, generator: BFieldElement, values: &[FF]) -> Self
    where
        S: Clone + One + Inverse,
        FF: Mul<S, Output = FF>,
    {
        let length = values.len();
        let mut mut_values = values.to_vec();

        intt(&mut mut_values, generator, length.ilog2());
        let poly = Polynomial::new(mut_values);

        poly.scale(offset.inverse())
    }

    /// Divide `self` by some `divisor`.
    ///
    /// # Panics
    ///
    /// Panics if the `divisor` is zero.
    pub fn divide(&self, divisor: &Self) -> (Self, Self) {
        const FAST_DIVIDE_THRESHOLD: isize = 1000;
        if self.degree() > FAST_DIVIDE_THRESHOLD {
            self.fast_divide(divisor)
        } else {
            self.naive_divide(divisor)
        }
    }

    /// Compute a polynomial g(X) from a given polynomial f(X) such that
    /// g(X) * f(X) = 1 mod X^n , where n is the precision.
    ///
    /// In formal terms, g(X) is the approximate multiplicative inverse in
    /// the formal power series ring FF[[X]], where elements obey the same
    /// algebraic rules as polynomials do but can have an infinite number of
    /// coefficients. To represent these elements on a computer, one has to
    /// truncate the coefficient vectors somewhere. The resulting truncation
    /// error is considered "small" when it lives on large powers of X. This
    /// function works by applying Newton's method in this ring.
    ///
    /// @panics when f(X) is not invertible in the formal power series ring,
    /// _i.e._, when its constant coefficient is zero.
    pub fn formal_power_series_inverse_newton(&self, precision: usize) -> Polynomial<FF> {
        // nonzero polynomials of degree zero have an exact inverse
        if self.degree() == 0 {
            return Polynomial::from_constant(self.coefficients[0].inverse());
        }

        // otherwise we need to run some iterations of Newton's method
        let num_rounds = precision.next_power_of_two().ilog2();

        // for small polynomials we use standard multiplication,
        // but for larger ones we want to stay in the ntt domain
        let switch_point = if Self::FORMAL_POWER_SERIES_INVERSE_CUTOFF < self.degree() as usize {
            0
        } else {
            (Self::FORMAL_POWER_SERIES_INVERSE_CUTOFF / self.degree() as usize).ilog2()
        };

        let cc = self.coefficients[0];

        // standard part
        let mut f = Polynomial::from_constant(cc.inverse());
        for _ in 0..u32::min(num_rounds, switch_point) {
            let sub = f.multiply(&f).multiply(self);
            f.scalar_mul_mut(FF::from(2));
            f = f - sub;
        }

        // if we already have the required precision, terminate early
        if switch_point >= num_rounds {
            return f;
        }

        // ntt-based multiplication from here on out

        // final NTT domain
        let full_domain_length =
            ((1 << (num_rounds + 1)) * self.degree() as usize).next_power_of_two();
        let full_omega = BFieldElement::primitive_root_of_unity(full_domain_length as u64).unwrap();
        let log_full_domain_length = full_domain_length.ilog2();

        let mut self_ntt = self.coefficients.clone();
        self_ntt.resize(full_domain_length, FF::zero());
        ntt(&mut self_ntt, full_omega, log_full_domain_length);

        // while possible we calculate over a smaller domain
        let mut current_domain_length = f.coefficients.len().next_power_of_two();

        // migrate to a larger domain as necessary
        let lde = |v: &mut [FF], old_domain_length: usize, new_domain_length: usize| {
            intt(
                &mut v[..old_domain_length],
                BFieldElement::primitive_root_of_unity(old_domain_length as u64).unwrap(),
                old_domain_length.ilog2(),
            );
            ntt(
                &mut v[..new_domain_length],
                BFieldElement::primitive_root_of_unity(new_domain_length as u64).unwrap(),
                new_domain_length.ilog2(),
            );
        };

        // use degree to track when domain-changes are necessary
        let mut f_degree = f.degree();
        let self_degree = self.degree();

        // allocate enough space for f and set initial values of elements used later to zero
        let mut f_ntt = f.coefficients;
        f_ntt.resize(full_domain_length, FF::from(0));
        ntt(
            &mut f_ntt[..current_domain_length],
            BFieldElement::primitive_root_of_unity(current_domain_length as u64).unwrap(),
            current_domain_length.ilog2(),
        );

        for _ in switch_point..num_rounds {
            f_degree = 2 * f_degree + self_degree;
            if f_degree as usize >= current_domain_length {
                let next_domain_length = (1 + f_degree as usize).next_power_of_two();
                lde(&mut f_ntt, current_domain_length, next_domain_length);
                current_domain_length = next_domain_length;
            }
            f_ntt
                .iter_mut()
                .zip(
                    self_ntt
                        .iter()
                        .step_by(full_domain_length / current_domain_length),
                )
                .for_each(|(ff, dd)| *ff = FF::from(2) * *ff - *ff * *ff * *dd);
        }

        intt(
            &mut f_ntt[..current_domain_length],
            BFieldElement::primitive_root_of_unity(current_domain_length as u64).unwrap(),
            current_domain_length.ilog2(),
        );
        Polynomial::new(f_ntt)
    }

    /// Given a polynomial f(X), find the polynomial g(X) of degree at most n
    /// such that f(X) * g(X) = 1 mod X^{n+1} where n is the precision.
    /// @panics if f(X) does not have an inverse in the formal power series
    /// ring, _i.e._ if its constant coefficient is zero.
    fn formal_power_series_inverse_minimal(&self, precision: usize) -> Self {
        let lc_inv = self.coefficients.first().unwrap().inverse();
        let mut g = vec![lc_inv];

        // invariant: product[i] = 0
        for _ in 1..(precision + 1) {
            let inner_product = self
                .coefficients
                .iter()
                .skip(1)
                .take(g.len())
                .zip(g.iter().rev())
                .map(|(l, r)| *l * *r)
                .fold(FF::from(0), |l, r| l + r);
            g.push(-inner_product * lc_inv);
        }

        Polynomial::new(g)
    }

    pub fn reverse(&self) -> Self {
        let degree = self.degree();
        Self::new(
            self.coefficients
                .iter()
                .take((degree + 1) as usize)
                .copied()
                .rev()
                .collect_vec(),
        )
    }

    /// Divide (with remainder) and throw away the quotient.
    fn reduce(&self, modulus: &Self) -> Self {
        let (_quotient, remainder) = self.divide(modulus);
        remainder
    }

    /// Given a polynomial to be reduced, a modulus, and a structured multiple
    /// of that modulus, compute the remainder after division by that modulus.
    /// This method is faster than [`reduce`](reduce) even with the
    /// preprocessing step (in which the structured multiple is calculated) but
    /// the point is that for many cost calculations the main processing step
    /// dominates.
    pub fn reduce_with_structured_multiple(&self, modulus: &Self, multiple: &Self) -> Self {
        let degree = modulus.degree();
        if degree == -1 {
            panic!("Cannot reduce by zero.");
        } else if degree == 0 {
            return Self::zero();
        } else if self.degree() < modulus.degree() {
            return self.clone();
        }

        let somewhat_reduced = self.reduce_by_structured_modulus(multiple);
        somewhat_reduced.reduce(modulus)
    }

    /// Given a polynomial f(X) of degree n, find a multiple of f(X) of the form
    /// X^{3*n+1} + (something of degree at most 2*n).
    /// @panics if f(X) = 0.
    fn structured_multiple(&self) -> Self {
        let n = self.degree();
        let reverse = self.reverse();
        let inverse_reverse = reverse.formal_power_series_inverse_minimal(n as usize);
        let product_reverse = reverse.multiply(&inverse_reverse);
        product_reverse.reverse()
    }

    /// Given a polynomial f(X) and an integer n, find a multiple of f(X) of the
    /// form X^n + (something of much smaller degree).
    ///
    /// @panics if the polynomial is zero, or if its degree is larger than n
    pub fn structured_multiple_of_degree(&self, n: usize) -> Self {
        if self.is_zero() {
            panic!("Cannot compute multiples of zero.");
        }
        let degree = self.degree() as usize;
        if degree > n {
            panic!("Cannot compute multiple of smaller degree.");
        }
        if degree == 0 {
            return Polynomial::new(
                [vec![FF::from(0); n], vec![self.coefficients[0].inverse()]].concat(),
            );
        }
        let reverse = self.reverse();
        // The next function gives back a polynomial g(X) of degree at most arg,
        // such that f(X) * g(X) = 1 mod X^arg.
        // Without modular reduction, the degree of the product f(X) * g(X) is
        // deg(f) + arg -- even after coefficient reversal. So n = deg(f) + arg
        // and arg = n - deg(f).
        let inverse_reverse = reverse.formal_power_series_inverse_minimal(n - degree);
        let product_reverse = reverse.multiply(&inverse_reverse);
        product_reverse.reverse()
    }

    /// Reduces f(X) by a structured modulus, which is of the form
    /// X^{m+n} + (something of degree less than m). When the modulus has this
    /// form, polynomial modular reductions can be computed faster than in the
    /// generic case.
    fn reduce_by_structured_modulus(&self, multiple: &Self) -> Self {
        let leading_term = Polynomial::new(
            [
                vec![FF::from(0); multiple.degree() as usize],
                vec![FF::from(1); 1],
            ]
            .concat(),
        );
        let shift_polynomial = multiple.clone() - leading_term.clone();
        let m = shift_polynomial.degree() as usize + 1;
        let n = multiple.degree() as usize - m;
        if self.coefficients.len() < n + m {
            return self.clone();
        }
        let num_reducible_chunks = (self.coefficients.len() - (m + n) + n - 1) / n;

        let range_start = num_reducible_chunks * n;
        let mut working_window = if range_start >= self.coefficients.len() {
            vec![FF::from(0); n + m]
        } else {
            self.coefficients[range_start..].to_vec()
        };
        working_window.resize(n + m, FF::from(0));

        for chunk_index in (0..num_reducible_chunks).rev() {
            let overflow = Polynomial::new(working_window[m..].to_vec());
            let product = overflow.multiply(&shift_polynomial);

            working_window = [vec![FF::from(0); n], working_window[0..m].to_vec()].concat();
            for (i, wwi) in working_window.iter_mut().enumerate().take(n) {
                *wwi = self.coefficients[chunk_index * n + i];
            }

            for (i, wwi) in working_window.iter_mut().enumerate().take(n + m) {
                *wwi -= *product.coefficients.get(i).unwrap_or(&FF::from(0));
            }
        }

        Polynomial::new(working_window)
    }

    /// Reduces f(X) by a structured modulus, which is of the form
    /// X^{m+n} + (something of degree less than m). When the modulus has this
    /// form, polynomial modular reductions can be computed faster than in the
    /// generic case.
    ///
    /// This method uses NTT-based multiplication, meaning that the unstructured
    /// part of the structured multiple must be given in NTT-domain.
    fn reduce_by_ntt_friendly_modulus(&self, shift_ntt: &[FF], m: usize) -> Self {
        let domain_length = shift_ntt.len();
        assert!(domain_length.is_power_of_two());
        let log_domain_length = domain_length.ilog2();
        let omega = BFieldElement::primitive_root_of_unity(domain_length as u64).unwrap();
        let n = domain_length - m;

        if self.coefficients.len() < n + m {
            return self.clone();
        }
        let num_reducible_chunks = (self.coefficients.len() - (m + n) + n - 1) / n;

        let range_start = num_reducible_chunks * n;
        let mut working_window = if range_start >= self.coefficients.len() {
            vec![FF::from(0); n + m]
        } else {
            self.coefficients[range_start..].to_vec()
        };
        working_window.resize(n + m, FF::from(0));

        for chunk_index in (0..num_reducible_chunks).rev() {
            let mut product = [working_window[m..].to_vec(), vec![FF::from(0); m]].concat();
            ntt(&mut product, omega, log_domain_length);
            product
                .iter_mut()
                .zip(shift_ntt.iter())
                .for_each(|(l, r)| *l *= *r);
            intt(&mut product, omega, log_domain_length);

            working_window = [vec![FF::from(0); n], working_window[0..m].to_vec()].concat();
            for (i, wwi) in working_window.iter_mut().enumerate().take(n) {
                *wwi = self.coefficients[chunk_index * n + i];
            }

            for (i, wwi) in working_window.iter_mut().enumerate().take(n + m) {
                *wwi -= product[i];
            }
        }

        Polynomial::new(working_window)
    }

    /// Compute the remainder after division of one polynomial by another. This
    /// method first reduces the numerator by a multiple of the denominator that
    /// was constructed to enable NTT-based chunk-wise reduction, before
    /// invoking the standard long division based algorithm to finalize. As a
    /// result, it works best for large numerators being reduced by small
    /// denominators.
    pub fn fast_reduce(&self, modulus: &Self) -> Self {
        if modulus.degree() == 0 {
            return Self::zero();
        }
        if self.degree() < modulus.degree() {
            return self.clone();
        }

        // 1. Chunk-wise reduction in NTT domain.
        // We generate a structured multiple of the modulus of the form
        // 1, (many zeros), *, *, *, *, *; where
        //                  -------------
        //                        |- m coefficients
        //    ---------------------------
        //               |- n=2^k coefficients.
        // This allows us to reduce the numerator's coefficients in chunks of
        // n-m using NTT-based multiplication over a domain of size n = 2^k.
        let n = usize::max(
            Polynomial::<FF>::FAST_REDUCE_CUTOFF_THRESHOLD,
            modulus.degree() as usize * 2,
        )
        .next_power_of_two();
        let ntt_friendly_multiple = modulus.structured_multiple_of_degree(n);
        // m = 1 + degree(ntt_friendly_multiple - leading term)
        let m = 1 + ntt_friendly_multiple
            .coefficients
            .iter()
            .enumerate()
            .rev()
            .skip(1)
            .find_map(|(i, c)| if !c.is_zero() { Some(i) } else { None })
            .unwrap_or(0);
        let mut shift_factor_ntt = ntt_friendly_multiple.coefficients[..n].to_vec();
        ntt(
            &mut shift_factor_ntt,
            BFieldElement::primitive_root_of_unity(n as u64).unwrap(),
            n.ilog2(),
        );
        let mut intermediate_remainder = self.reduce_by_ntt_friendly_modulus(&shift_factor_ntt, m);

        // 2. Chunk-wise reduction with schoolbook multiplication.
        // We generate a smaller structured multiple of the denominator that
        // that also admits chunk-wise reduction but not NTT-based
        // multiplication within. While asymptotically on par with long
        // division, this schoolbook chunk-wise reduction is concretely more
        // performant.
        if intermediate_remainder.degree() > 4 * modulus.degree() {
            let structured_multiple = modulus.structured_multiple();
            intermediate_remainder =
                intermediate_remainder.reduce_by_structured_modulus(&structured_multiple);
        }

        // 3. Long division based reduction by the (unmultiplied) modulus.
        intermediate_remainder.reduce(modulus)
    }

    /// Polynomial long division with `self` as the dividend, divided by some `divisor`.
    /// Only `pub` to allow benchmarking; not considered part of the public API.
    ///
    /// As the name implies, the advantage of this method over [`divide`](Self::naive_divide) is
    /// runtime complexity. Concretely, this method has time complexity in O(n·log(n)), whereas
    /// [`divide`](Self::naive_divide) has time complexity in O(n^2).
    #[doc(hidden)]
    pub fn fast_divide(&self, divisor: &Self) -> (Self, Self) {
        // The math for this function: [0]. There are very slight deviations, for example around the
        // requirement that the divisor is monic.
        //
        // [0] https://cs.uwaterloo.ca/~r5olivei/courses/2021-winter-cs487/lecture5-post.pdf

        let Ok(quotient_degree) = usize::try_from(self.degree() - divisor.degree()) else {
            return (Self::zero(), self.clone());
        };

        if divisor.degree() == 0 {
            let quotient = self.scalar_mul(divisor.leading_coefficient().unwrap().inverse());
            let remainder = Polynomial::zero();
            return (quotient, remainder);
        }

        // Reverse coefficient vectors to move into formal power series ring over FF, i.e., FF[[x]].
        // Re-interpret as a polynomial to benefit from the already-implemented multiplication
        // method, which mechanically work the same in FF[X] and FF[[x]].
        let reverse = |poly: &Self| Self::new(poly.coefficients.iter().copied().rev().collect());

        // Newton iteration to invert divisor up to required precision. Why is this the required
        // precision? Good question.
        let precision = (quotient_degree + 1).next_power_of_two();

        let rev_divisor = reverse(divisor);
        let rev_divisor_inverse = rev_divisor.formal_power_series_inverse_newton(precision);

        let self_reverse = reverse(self);
        let rev_quotient = self_reverse.multiply(&rev_divisor_inverse);

        let quotient = reverse(&rev_quotient).truncate(quotient_degree);

        let remainder = self.clone() - quotient.multiply(divisor);
        (quotient, remainder)
    }

    /// The degree-`k` polynomial with the same `k + 1` leading coefficients as `self`. To be more
    /// precise: The degree of the result will be the minimum of `k` and [`Self::degree()`]. This
    /// implies, among other things, that if `self` [is zero](Self::is_zero()), the result will also
    /// be zero, independent of `k`.
    ///
    /// # Examples
    ///
    /// ```
    /// # use twenty_first::prelude::*;
    /// let f = Polynomial::new(bfe_vec![0, 1, 2, 3, 4]); // 4x⁴ + 3x³ + 2x² + 1x¹ + 0
    /// let g = f.truncate(2);                            // 4x² + 3x¹ + 2
    /// assert_eq!(Polynomial::new(bfe_vec![2, 3, 4]), g);
    /// ```
    pub fn truncate(&self, k: usize) -> Self {
        let coefficients = self.coefficients.iter().copied();
        let coefficients = coefficients.rev().take(k + 1).rev().collect();
        Self::new(coefficients)
    }

    /// `self % x^n`
    ///
    /// A special case of [Self::rem], and faster.
    ///
    /// # Examples
    ///
    /// ```
    /// # use twenty_first::prelude::*;
    /// let f = Polynomial::new(bfe_vec![0, 1, 2, 3, 4]); // 4x⁴ + 3x³ + 2x² + 1x¹ + 0
    /// let g = f.mod_x_to_the_n(2);                      // 1x¹ + 0
    /// assert_eq!(Polynomial::new(bfe_vec![0, 1]), g);
    /// ```
    pub fn mod_x_to_the_n(&self, n: usize) -> Self {
        let num_coefficients_to_retain = n.min(self.coefficients.len());
        Self::new(self.coefficients[..num_coefficients_to_retain].into())
    }
}

impl Polynomial<BFieldElement> {
    /// [Clean division](Self::clean_divide) is slower than [naïve divison](Self::naive_divide) for
    /// polynomials of degree less than this threshold.
    ///
    /// Extracted from `cargo bench --bench poly_clean_div` on mjolnir.
    const CLEAN_DIVIDE_CUTOFF_THRESHOLD: isize = {
        if cfg!(test) {
            0
        } else {
            1 << 9
        }
    };

    /// A fast way of dividing two polynomials. Only works if division is clean, _i.e._, if the
    /// remainder of polynomial long division is [zero]. This **must** be known ahead of time. If
    /// division is unclean, this method might panic or produce a wrong result.
    /// Use [`Polynomial::divide`] for more generality.
    ///
    /// # Panics
    ///
    /// Panics if
    /// - the divisor is [zero], or
    /// - division is not clean, _i.e._, if polynomial long division leaves some non-zero remainder.
    ///
    /// [zero]: Polynomial::is_zero
    #[must_use]
    pub fn clean_divide(mut self, mut divisor: Self) -> Self {
        if divisor.degree() < Self::CLEAN_DIVIDE_CUTOFF_THRESHOLD {
            return self.divide(&divisor).0;
        }

        // Incompleteness workaround: Manually check whether 0 is a root of the divisor.
        // f(0) == 0 <=> f's constant term is 0
        if divisor.coefficients.first().is_some_and(Zero::is_zero) {
            // Clean division implies the dividend also has 0 as a root.
            assert!(self.coefficients[0].is_zero());
            self.coefficients.remove(0);
            divisor.coefficients.remove(0);
        }

        // Incompleteness workaround: Move both dividend and divisor to an extension field.
        let offset = XFieldElement::from([0, 1, 0]);
        let mut dividend_coefficients = self.scale(offset).coefficients;
        let mut divisor_coefficients = divisor.scale(offset).coefficients;

        // See the comment in `fast_coset_evaluate` why this bound is necessary.
        let dividend_deg_plus_1 = usize::try_from(self.degree() + 1).unwrap();
        let order = dividend_deg_plus_1.next_power_of_two();
        let order_u64 = u64::try_from(order).unwrap();
        let root = BFieldElement::primitive_root_of_unity(order_u64).unwrap();

        dividend_coefficients.resize(order, XFieldElement::zero());
        divisor_coefficients.resize(order, XFieldElement::zero());

        ntt(&mut dividend_coefficients, root, order.ilog2());
        ntt(&mut divisor_coefficients, root, order.ilog2());

        let divisor_inverses = XFieldElement::batch_inversion(divisor_coefficients);
        let mut quotient_codeword = dividend_coefficients
            .into_iter()
            .zip(divisor_inverses)
            .map(|(l, r)| l * r)
            .collect_vec();

        intt(&mut quotient_codeword, root, order.ilog2());
        let quotient = Polynomial::new(quotient_codeword);

        // If the division was clean, “unscaling” brings all coefficients back to the base field.
        let quotient = quotient.scale(offset.inverse());
        let coeffs = quotient.coefficients.into_iter();
        coeffs.map(|c| c.unlift().unwrap()).collect_vec().into()
    }
}

impl<const N: usize, FF, E> From<[E; N]> for Polynomial<FF>
where
    FF: FiniteField,
    E: Into<FF>,
{
    fn from(coefficients: [E; N]) -> Self {
        Self::new(coefficients.into_iter().map(|x| x.into()).collect())
    }
}

impl<FF, E> From<&[E]> for Polynomial<FF>
where
    FF: FiniteField,
    E: Into<FF> + Clone,
{
    fn from(coefficients: &[E]) -> Self {
        Self::from(coefficients.to_vec())
    }
}

impl<FF, E> From<Vec<E>> for Polynomial<FF>
where
    FF: FiniteField,
    E: Into<FF>,
{
    fn from(coefficients: Vec<E>) -> Self {
        Self::new(coefficients.into_iter().map(|c| c.into()).collect())
    }
}

impl<FF, E> From<&Vec<E>> for Polynomial<FF>
where
    FF: FiniteField,
    E: Into<FF> + Clone,
{
    fn from(coefficients: &Vec<E>) -> Self {
        Self::from(coefficients.to_vec())
    }
}

impl<FF: FiniteField> Polynomial<FF> {
    pub const fn new(coefficients: Vec<FF>) -> Self {
        Self { coefficients }
    }

    pub fn normalize(&mut self) {
        while !self.coefficients.is_empty() && self.coefficients.last().unwrap().is_zero() {
            self.coefficients.pop();
        }
    }

    pub fn from_constant(constant: FF) -> Self {
        Self {
            coefficients: vec![constant],
        }
    }

    pub fn is_x(&self) -> bool {
        self.degree() == 1 && self.coefficients[0].is_zero() && self.coefficients[1].is_one()
    }

    pub fn evaluate(&self, x: FF) -> FF {
        let mut acc = FF::zero();
        for &c in self.coefficients.iter().rev() {
            acc = c + x * acc;
        }

        acc
    }

    /// The coefficient of the polynomial's term of highest power. `None` if (and only if) `self`
    /// [is zero](Self::is_zero).
    ///
    /// Furthermore, is never `Some(FF::zero())`.
    ///
    /// # Examples
    ///
    /// ```
    /// # use twenty_first::prelude::*;
    /// # use num_traits::Zero;
    /// let f = Polynomial::new(bfe_vec![1, 2, 3]);
    /// assert_eq!(Some(bfe!(3)), f.leading_coefficient());
    /// assert_eq!(None, Polynomial::<XFieldElement>::zero().leading_coefficient());
    /// ```
    pub fn leading_coefficient(&self) -> Option<FF> {
        match self.degree() {
            -1 => None,
            n => Some(self.coefficients[n as usize]),
        }
    }

    pub fn are_colinear_3(p0: (FF, FF), p1: (FF, FF), p2: (FF, FF)) -> bool {
        if p0.0 == p1.0 || p1.0 == p2.0 || p2.0 == p0.0 {
            return false;
        }

        let dy = p0.1 - p1.1;
        let dx = p0.0 - p1.0;

        dx * (p2.1 - p0.1) == dy * (p2.0 - p0.0)
    }

    pub fn get_colinear_y(p0: (FF, FF), p1: (FF, FF), p2_x: FF) -> FF {
        assert_ne!(p0.0, p1.0, "Line must not be parallel to y-axis");
        let dy = p0.1 - p1.1;
        let dx = p0.0 - p1.0;
        let p2_y_times_dx = dy * (p2_x - p0.0) + dx * p0.1;

        // Can we implement this without division?
        p2_y_times_dx / dx
    }

    /// Only `pub` to allow benchmarking; not considered part of the public API.
    #[doc(hidden)]
    pub fn naive_zerofier(domain: &[FF]) -> Self {
        domain
            .iter()
            .map(|&r| Self::new(vec![-r, FF::one()]))
            .reduce(|accumulator, linear_poly| accumulator * linear_poly)
            .unwrap_or_else(Self::one)
    }

    /// Slow square implementation that does not use NTT
    #[must_use]
    pub fn slow_square(&self) -> Self {
        let degree = self.degree();
        if degree == -1 {
            return Self::zero();
        }

        let squared_coefficient_len = self.degree() as usize * 2 + 1;
        let zero = FF::zero();
        let one = FF::one();
        let two = one + one;
        let mut squared_coefficients = vec![zero; squared_coefficient_len];

        for i in 0..self.coefficients.len() {
            let ci = self.coefficients[i];
            squared_coefficients[2 * i] += ci * ci;

            // TODO: Review.
            for j in i + 1..self.coefficients.len() {
                let cj = self.coefficients[j];
                squared_coefficients[i + j] += two * ci * cj;
            }
        }

        Self {
            coefficients: squared_coefficients,
        }
    }
}

impl<FF: FiniteField> Polynomial<FF> {
    pub fn are_colinear(points: &[(FF, FF)]) -> bool {
        if points.len() < 3 {
            return false;
        }

        if !points.iter().map(|(x, _)| x).all_unique() {
            return false;
        }

        // Find 1st degree polynomial through first two points
        let (p0_x, p0_y) = points[0];
        let (p1_x, p1_y) = points[1];
        let a = (p0_y - p1_y) / (p0_x - p1_x);
        let b = p0_y - a * p0_x;

        points.iter().skip(2).all(|&(x, y)| a * x + b == y)
    }
}

impl<FF: FiniteField> Polynomial<FF> {
    /// Only `pub` to allow benchmarking; not considered part of the public API.
    #[doc(hidden)]
    pub fn naive_multiply(&self, other: &Self) -> Self {
        let Ok(degree_lhs) = usize::try_from(self.degree()) else {
            return Self::zero();
        };
        let Ok(degree_rhs) = usize::try_from(other.degree()) else {
            return Self::zero();
        };

        let mut product = vec![FF::zero(); degree_lhs + degree_rhs + 1];
        for i in 0..=degree_lhs {
            for j in 0..=degree_rhs {
                product[i + j] += self.coefficients[i] * other.coefficients[j];
            }
        }

        Self::new(product)
    }

    /// Multiply a polynomial with itself `pow` times
    #[must_use]
    pub fn mod_pow(&self, pow: BigInt) -> Self {
        let one = FF::one();

        // Special case to handle 0^0 = 1
        if pow.is_zero() {
            return Self::from_constant(one);
        }

        if self.is_zero() {
            return Self::zero();
        }

        let mut acc = Polynomial::from_constant(one);
        let bit_length: u64 = pow.bits();
        for i in 0..bit_length {
            acc = acc.slow_square();
            let set: bool =
                !(pow.clone() & Into::<BigInt>::into(1u128 << (bit_length - 1 - i))).is_zero();
            if set {
                acc = acc * self.clone();
            }
        }

        acc
    }

    pub fn shift_coefficients_mut(&mut self, power: usize, zero: FF) {
        self.coefficients.splice(0..0, vec![zero; power]);
    }

    /// Multiply a polynomial with x^power
    #[must_use]
    pub fn shift_coefficients(&self, power: usize) -> Self {
        let zero = FF::zero();

        let mut coefficients: Vec<FF> = self.coefficients.clone();
        coefficients.splice(0..0, vec![zero; power]);
        Self { coefficients }
    }

    /// Multiply a polynomial with a scalar, _i.e._, compute `scalar · self(x)`.
    ///
    /// Slightly faster but slightly less general than [`Self::scalar_mul`].
    ///
    /// # Examples
    ///
    /// ```
    /// # use twenty_first::prelude::*;
    /// let mut f = Polynomial::new(bfe_vec![1, 2, 3]);
    /// f.scalar_mul_mut(bfe!(2));
    /// assert_eq!(Polynomial::new(bfe_vec![2, 4, 6]), f);
    /// ```
    pub fn scalar_mul_mut<S>(&mut self, scalar: S)
    where
        S: Clone,
        FF: MulAssign<S>,
    {
        for coefficient in &mut self.coefficients {
            *coefficient *= scalar.clone();
        }
    }

    /// Multiply a polynomial with a scalar, _i.e._, compute `scalar · self(x)`.
    ///
    /// Slightly slower but slightly more general than [`Self::scalar_mul_mut`].
    ///
    /// # Examples
    ///
    /// ```
    /// # use twenty_first::prelude::*;
    /// let f = Polynomial::new(bfe_vec![1, 2, 3]);
    /// let g = f.scalar_mul(bfe!(2));
    /// assert_eq!(Polynomial::new(bfe_vec![2, 4, 6]), g);
    /// ```
    #[must_use]
    pub fn scalar_mul<S, FF2>(&self, scalar: S) -> Polynomial<FF2>
    where
        S: Clone,
        FF: Mul<S, Output = FF2>,
        FF2: FiniteField,
    {
        let coeff_iter = self.coefficients.iter();
        let new_coeffs = coeff_iter.map(|&c| c * scalar.clone()).collect();
        Polynomial::new(new_coeffs)
    }

    /// Return (quotient, remainder).
    ///
    /// Only `pub` to allow benchmarking; not considered part of the public API.
    #[doc(hidden)]
    pub fn naive_divide(&self, divisor: &Self) -> (Self, Self) {
        let divisor_lc_inv = divisor
            .leading_coefficient()
            .expect("divisor should be non-zero")
            .inverse();

        let Ok(quotient_degree) = usize::try_from(self.degree() - divisor.degree()) else {
            // self.degree() < divisor.degree()
            return (Self::zero(), self.to_owned());
        };
        debug_assert!(!self.is_zero());

        // quotient is built from back to front, must be reversed later
        let mut rev_quotient = Vec::with_capacity(quotient_degree + 1);
        let mut remainder = self.clone();
        remainder.normalize();

        // The divisor is also iterated back to front.
        // It is normalized manually to avoid it being a `&mut` argument.
        let rev_divisor = divisor.coefficients.iter().rev();
        let normal_rev_divisor = rev_divisor.skip_while(|c| c.is_zero());

        for _ in 0..=quotient_degree {
            let remainder_lc = remainder.coefficients.pop().unwrap();
            let quotient_coeff = remainder_lc * divisor_lc_inv;
            rev_quotient.push(quotient_coeff);

            if quotient_coeff.is_zero() {
                continue;
            }

            // don't use `.degree()` to still count leading zeros in intermittent remainders
            let remainder_degree = remainder.coefficients.len().saturating_sub(1);

            // skip divisor's leading coefficient: it has already been dealt with
            for (i, &divisor_coeff) in normal_rev_divisor.clone().skip(1).enumerate() {
                remainder.coefficients[remainder_degree - i] -= quotient_coeff * divisor_coeff;
            }
        }

        rev_quotient.reverse();
        let quotient = Self::new(rev_quotient);

        (quotient, remainder)
    }
}

impl<FF: FiniteField> Div for Polynomial<FF> {
    type Output = Self;

    fn div(self, other: Self) -> Self {
        let (quotient, _): (Self, Self) = self.naive_divide(&other);
        quotient
    }
}

impl<FF: FiniteField> Rem for Polynomial<FF> {
    type Output = Self;

    fn rem(self, other: Self) -> Self {
        let (_, remainder): (Self, Self) = self.naive_divide(&other);
        remainder
    }
}

impl<FF: FiniteField> Add for Polynomial<FF> {
    type Output = Self;

    fn add(self, other: Self) -> Self {
        let summed: Vec<FF> = self
            .coefficients
            .into_iter()
            .zip_longest(other.coefficients)
            .map(|a| match a {
                EitherOrBoth::Both(l, r) => l.to_owned() + r.to_owned(),
                EitherOrBoth::Left(l) => l.to_owned(),
                EitherOrBoth::Right(r) => r.to_owned(),
            })
            .collect();

        Self {
            coefficients: summed,
        }
    }
}

impl<FF: FiniteField> AddAssign for Polynomial<FF> {
    fn add_assign(&mut self, rhs: Self) {
        let rhs_len = rhs.coefficients.len();
        let self_len = self.coefficients.len();
        for i in 0..std::cmp::min(self_len, rhs_len) {
            self.coefficients[i] = self.coefficients[i] + rhs.coefficients[i];
        }

        if rhs_len > self_len {
            self.coefficients
                .append(&mut rhs.coefficients[self_len..].to_vec());
        }
    }
}

impl<FF: FiniteField> Sub for Polynomial<FF> {
    type Output = Self;

    fn sub(self, other: Self) -> Self {
        let coefficients = self
            .coefficients
            .into_iter()
            .zip_longest(other.coefficients)
            .map(|a| match a {
                EitherOrBoth::Both(l, r) => l - r,
                EitherOrBoth::Left(l) => l,
                EitherOrBoth::Right(r) => FF::zero() - r,
            })
            .collect();

        Self { coefficients }
    }
}

impl<FF: FiniteField> Polynomial<FF> {
    /// Extended Euclidean algorithm with polynomials. Computes the greatest
    /// common divisor `gcd` as a monic polynomial, as well as the corresponding
    /// Bézout coefficients `a` and `b`, satisfying `gcd = a·x + b·y`
    ///
    /// # Example
    ///
    /// ```
    /// # use twenty_first::prelude::Polynomial;
    /// # use twenty_first::prelude::BFieldElement;
    /// let x = Polynomial::<BFieldElement>::from([1, 0, 1]);
    /// let y = Polynomial::<BFieldElement>::from([1, 1]);
    /// let (gcd, a, b) = Polynomial::xgcd(x.clone(), y.clone());
    /// assert_eq!(gcd, a * x + b * y);
    /// ```
    pub fn xgcd(mut x: Self, mut y: Self) -> (Self, Self, Self) {
        let (mut a_factor, mut a1) = (Self::one(), Self::zero());
        let (mut b_factor, mut b1) = (Self::zero(), Self::one());

        while !y.is_zero() {
            let (quotient, remainder) = x.naive_divide(&y);
            let c = a_factor - quotient.clone() * a1.clone();
            let d = b_factor - quotient * b1.clone();

            x = y;
            y = remainder;
            a_factor = a1;
            a1 = c;
            b_factor = b1;
            b1 = d;
        }

        // normalize result to ensure the gcd, _i.e._, `x` has leading coefficient 1
        let lc = x.leading_coefficient().unwrap_or_else(FF::one);
        let normalize = |mut poly: Self| {
            poly.scalar_mul_mut(lc.inverse());
            poly
        };

        let [x, a, b] = [x, a_factor, b_factor].map(normalize);
        (x, a, b)
    }
}

impl<FF: FiniteField> Polynomial<FF> {
    pub fn degree(&self) -> isize {
        let mut deg = self.coefficients.len() as isize - 1;
        while deg >= 0 && self.coefficients[deg as usize].is_zero() {
            deg -= 1;
        }

        deg // -1 for the zero polynomial
    }

    pub fn formal_derivative(&self) -> Self {
        // not `enumerate()`ing: `FiniteField` is trait-bound to `From<u64>` but not `From<usize>`
        let coefficients = (0..)
            .zip(&self.coefficients)
            .map(|(i, &coefficient)| FF::from(i) * coefficient)
            .skip(1)
            .collect();

        Self { coefficients }
    }
}

impl<FF: FiniteField> Mul for Polynomial<FF> {
    type Output = Self;

    fn mul(self, other: Self) -> Self {
        self.naive_multiply(&other)
    }
}

impl<FF: FiniteField> Neg for Polynomial<FF> {
    type Output = Self;

    fn neg(mut self) -> Self::Output {
        self.scalar_mul_mut(-FF::one());
        self
    }
}

#[cfg(test)]
mod test_polynomials {
    use proptest::collection::size_range;
    use proptest::collection::vec;
    use proptest::prelude::*;
    use proptest_arbitrary_interop::arb;
    use rand::thread_rng;
    use test_strategy::proptest;

    use crate::prelude::*;

    use super::*;

    impl proptest::arbitrary::Arbitrary for Polynomial<BFieldElement> {
        type Parameters = ();

        fn arbitrary_with(_: Self::Parameters) -> Self::Strategy {
            arb().boxed()
        }

        type Strategy = BoxedStrategy<Self>;
    }

    impl proptest::arbitrary::Arbitrary for Polynomial<XFieldElement> {
        type Parameters = ();

        fn arbitrary_with(_: Self::Parameters) -> Self::Strategy {
            arb().boxed()
        }

        type Strategy = BoxedStrategy<Self>;
    }

    #[test]
    fn polynomial_display_test() {
        let polynomial = |cs: &[u64]| Polynomial::<BFieldElement>::from(cs);

        assert_eq!("0", polynomial(&[]).to_string());
        assert_eq!("0", polynomial(&[0]).to_string());
        assert_eq!("0", polynomial(&[0, 0]).to_string());

        assert_eq!("1", polynomial(&[1]).to_string());
        assert_eq!("2", polynomial(&[2, 0]).to_string());
        assert_eq!("3", polynomial(&[3, 0, 0]).to_string());

        assert_eq!("x", polynomial(&[0, 1]).to_string());
        assert_eq!("2x", polynomial(&[0, 2]).to_string());
        assert_eq!("3x", polynomial(&[0, 3]).to_string());

        assert_eq!("5x + 2", polynomial(&[2, 5]).to_string());
        assert_eq!("9x + 7", polynomial(&[7, 9, 0, 0, 0]).to_string());

        assert_eq!("4x^4 + 3x^3", polynomial(&[0, 0, 0, 3, 4]).to_string());
        assert_eq!("2x^4 + 1", polynomial(&[1, 0, 0, 0, 2]).to_string());
    }

    #[proptest]
    fn leading_coefficient_of_zero_polynomial_is_none(#[strategy(0usize..30)] num_zeros: usize) {
        let coefficients = vec![BFieldElement::zero(); num_zeros];
        let polynomial = Polynomial { coefficients };
        prop_assert!(polynomial.leading_coefficient().is_none());
    }

    #[proptest]
    fn leading_coefficient_of_non_zero_polynomial_is_some(
        polynomial: Polynomial<BFieldElement>,
        leading_coefficient: BFieldElement,
        #[strategy(0usize..30)] num_leading_zeros: usize,
    ) {
        let mut coefficients = polynomial.coefficients;
        coefficients.push(leading_coefficient);
        coefficients.extend(vec![BFieldElement::zero(); num_leading_zeros]);
        let polynomial_with_leading_zeros = Polynomial { coefficients };
        prop_assert_eq!(
            leading_coefficient,
            polynomial_with_leading_zeros.leading_coefficient().unwrap()
        );
    }

    #[test]
    fn normalizing_canonical_zero_polynomial_has_no_effect() {
        let mut zero_polynomial = Polynomial::<BFieldElement>::zero();
        zero_polynomial.normalize();
        assert_eq!(Polynomial::zero(), zero_polynomial);
    }

    #[proptest]
    fn spurious_leading_zeros_dont_affect_equality(
        polynomial: Polynomial<BFieldElement>,
        #[strategy(0usize..30)] num_leading_zeros: usize,
    ) {
        let mut coefficients = polynomial.coefficients.clone();
        coefficients.extend(vec![BFieldElement::zero(); num_leading_zeros]);
        let polynomial_with_leading_zeros = Polynomial { coefficients };

        prop_assert_eq!(polynomial, polynomial_with_leading_zeros);
    }

    #[proptest]
    fn normalizing_removes_spurious_leading_zeros(
        polynomial: Polynomial<BFieldElement>,
        #[filter(!#leading_coefficient.is_zero())] leading_coefficient: BFieldElement,
        #[strategy(0usize..30)] num_leading_zeros: usize,
    ) {
        let mut coefficients = polynomial.coefficients.clone();
        coefficients.push(leading_coefficient);
        coefficients.extend(vec![BFieldElement::zero(); num_leading_zeros]);
        let mut polynomial_with_leading_zeros = Polynomial { coefficients };
        polynomial_with_leading_zeros.normalize();

        let num_inserted_coefficients = 1;
        let expected_num_coefficients = polynomial.coefficients.len() + num_inserted_coefficients;
        let num_coefficients = polynomial_with_leading_zeros.coefficients.len();

        prop_assert_eq!(expected_num_coefficients, num_coefficients);
    }

    #[test]
    fn scaling_a_polynomial_works_with_different_fields_as_the_offset() {
        let bfe_poly = Polynomial::new(bfe_vec![0, 1, 2]);
        let _ = bfe_poly.scale(bfe!(42));
        let _ = bfe_poly.scale(xfe!(42));

        let xfe_poly = Polynomial::new(xfe_vec![0, 1, 2]);
        let _ = xfe_poly.scale(bfe!(42));
        let _ = xfe_poly.scale(xfe!(42));
    }

    #[proptest]
    fn polynomial_scaling_is_equivalent_in_extension_field(
        bfe_polynomial: Polynomial<BFieldElement>,
        alpha: BFieldElement,
    ) {
        let bfe_coefficients = bfe_polynomial.coefficients.iter();
        let xfe_coefficients = bfe_coefficients.map(|bfe| bfe.lift()).collect();
        let xfe_polynomial = Polynomial::<XFieldElement>::new(xfe_coefficients);

        let xfe_poly_bfe_scalar = xfe_polynomial.scale(alpha);
        let bfe_poly_xfe_scalar = bfe_polynomial.scale(alpha.lift());
        prop_assert_eq!(xfe_poly_bfe_scalar, bfe_poly_xfe_scalar);
    }

    #[proptest]
    fn evaluating_scaled_polynomial_is_equivalent_to_evaluating_original_in_offset_point(
        polynomial: Polynomial<BFieldElement>,
        alpha: BFieldElement,
        x: BFieldElement,
    ) {
        let scaled_polynomial = polynomial.scale(alpha);
        prop_assert_eq!(
            polynomial.evaluate(alpha * x),
            scaled_polynomial.evaluate(x)
        );
    }

    #[proptest]
    fn polynomial_multiplication_with_scalar_is_equivalent_for_the_two_methods(
        mut polynomial: Polynomial<BFieldElement>,
        scalar: BFieldElement,
    ) {
        let new_polynomial = polynomial.scalar_mul(scalar);
        polynomial.scalar_mul_mut(scalar);
        prop_assert_eq!(polynomial, new_polynomial);
    }

    #[test]
    fn polynomial_multiplication_with_scalar_works_for_various_types() {
        let bfe_poly = Polynomial::new(bfe_vec![0, 1, 2]);
        let _: Polynomial<BFieldElement> = bfe_poly.scalar_mul(bfe!(42));
        let _: Polynomial<XFieldElement> = bfe_poly.scalar_mul(xfe!(42));

        let xfe_poly = Polynomial::new(xfe_vec![0, 1, 2]);
        let _: Polynomial<XFieldElement> = xfe_poly.scalar_mul(bfe!(42));
        let _: Polynomial<XFieldElement> = xfe_poly.scalar_mul(xfe!(42));

        let mut bfe_poly = bfe_poly;
        bfe_poly.scalar_mul_mut(bfe!(42));

        let mut xfe_poly = xfe_poly;
        xfe_poly.scalar_mul_mut(bfe!(42));
        xfe_poly.scalar_mul_mut(xfe!(42));
    }

    #[proptest]
    fn slow_lagrange_interpolation(
        polynomial: Polynomial<BFieldElement>,
        #[strategy(Just(#polynomial.coefficients.len().max(1)))] _min_points: usize,
        #[any(size_range(#_min_points..8 * #_min_points).lift())] points: Vec<BFieldElement>,
    ) {
        let evaluations = points
            .into_iter()
            .map(|x| (x, polynomial.evaluate(x)))
            .collect_vec();
        let interpolation_polynomial = Polynomial::lagrange_interpolate_zipped(&evaluations);
        prop_assert_eq!(polynomial, interpolation_polynomial);
    }

    #[proptest]
    fn three_colinear_points_are_colinear(
        p0: (BFieldElement, BFieldElement),
        #[filter(#p0.0 != #p1.0)] p1: (BFieldElement, BFieldElement),
        #[filter(#p0.0 != #p2_x && #p1.0 != #p2_x)] p2_x: BFieldElement,
    ) {
        let line = Polynomial::lagrange_interpolate_zipped(&[p0, p1]);
        let p2 = (p2_x, line.evaluate(p2_x));
        prop_assert!(Polynomial::are_colinear_3(p0, p1, p2));
    }

    #[proptest]
    fn three_non_colinear_points_are_not_colinear(
        p0: (BFieldElement, BFieldElement),
        #[filter(#p0.0 != #p1.0)] p1: (BFieldElement, BFieldElement),
        #[filter(#p0.0 != #p2_x && #p1.0 != #p2_x)] p2_x: BFieldElement,
        #[filter(!#disturbance.is_zero())] disturbance: BFieldElement,
    ) {
        let line = Polynomial::lagrange_interpolate_zipped(&[p0, p1]);
        let p2 = (p2_x, line.evaluate(p2_x) + disturbance);
        prop_assert!(!Polynomial::are_colinear_3(p0, p1, p2));
    }

    #[proptest]
    fn colinearity_check_needs_at_least_three_points(
        p0: (BFieldElement, BFieldElement),
        #[filter(#p0.0 != #p1.0)] p1: (BFieldElement, BFieldElement),
    ) {
        prop_assert!(!Polynomial::<BFieldElement>::are_colinear(&[]));
        prop_assert!(!Polynomial::are_colinear(&[p0]));
        prop_assert!(!Polynomial::are_colinear(&[p0, p1]));
    }

    #[proptest]
    fn colinearity_check_with_repeated_points_fails(
        p0: (BFieldElement, BFieldElement),
        #[filter(#p0.0 != #p1.0)] p1: (BFieldElement, BFieldElement),
    ) {
        prop_assert!(!Polynomial::are_colinear(&[p0, p1, p1]));
    }

    #[proptest]
    fn colinear_points_are_colinear(
        p0: (BFieldElement, BFieldElement),
        #[filter(#p0.0 != #p1.0)] p1: (BFieldElement, BFieldElement),
        #[filter(!#additional_points_xs.contains(&#p0.0))]
        #[filter(!#additional_points_xs.contains(&#p1.0))]
        #[filter(#additional_points_xs.iter().all_unique())]
        #[any(size_range(1..100).lift())]
        additional_points_xs: Vec<BFieldElement>,
    ) {
        let line = Polynomial::lagrange_interpolate_zipped(&[p0, p1]);
        let additional_points = additional_points_xs
            .into_iter()
            .map(|x| (x, line.evaluate(x)))
            .collect_vec();
        let all_points = [p0, p1].into_iter().chain(additional_points).collect_vec();
        prop_assert!(Polynomial::are_colinear(&all_points));
    }

    #[test]
    #[should_panic(expected = "Line must not be parallel to y-axis")]
    fn getting_point_on_invalid_line_fails() {
        let one = BFieldElement::one();
        let two = one + one;
        let three = two + one;
        Polynomial::<BFieldElement>::get_colinear_y((one, one), (one, three), two);
    }

    #[proptest]
    fn point_on_line_and_colinear_point_are_identical(
        p0: (BFieldElement, BFieldElement),
        #[filter(#p0.0 != #p1.0)] p1: (BFieldElement, BFieldElement),
        x: BFieldElement,
    ) {
        let line = Polynomial::lagrange_interpolate_zipped(&[p0, p1]);
        let y = line.evaluate(x);
        let y_from_get_point_on_line = Polynomial::get_colinear_y(p0, p1, x);
        prop_assert_eq!(y, y_from_get_point_on_line);
    }

    #[proptest]
    fn point_on_line_and_colinear_point_are_identical_in_extension_field(
        p0: (XFieldElement, XFieldElement),
        #[filter(#p0.0 != #p1.0)] p1: (XFieldElement, XFieldElement),
        x: XFieldElement,
    ) {
        let line = Polynomial::lagrange_interpolate_zipped(&[p0, p1]);
        let y = line.evaluate(x);
        let y_from_get_point_on_line = Polynomial::get_colinear_y(p0, p1, x);
        prop_assert_eq!(y, y_from_get_point_on_line);
    }

    #[proptest]
    fn shifting_polynomial_coefficients_by_zero_is_the_same_as_not_shifting_it(
        poly: Polynomial<BFieldElement>,
    ) {
        prop_assert_eq!(poly.clone(), poly.shift_coefficients(0));
    }

    #[proptest]
    fn shifting_polynomial_one_is_equivalent_to_raising_polynomial_x_to_the_power_of_the_shift(
        #[strategy(0usize..30)] shift: usize,
    ) {
        let shifted_one = Polynomial::one().shift_coefficients(shift);
        let x_to_the_shift = Polynomial::<BFieldElement>::from([0, 1]).mod_pow(shift.into());
        prop_assert_eq!(shifted_one, x_to_the_shift);
    }

    #[test]
    fn polynomial_shift_test() {
        let polynomial = Polynomial::<BFieldElement>::from([17, 14]);
        assert_eq!(
            bfe_vec![17, 14],
            polynomial.shift_coefficients(0).coefficients
        );
        assert_eq!(
            bfe_vec![0, 17, 14],
            polynomial.shift_coefficients(1).coefficients
        );
        assert_eq!(
            bfe_vec![0, 0, 0, 0, 17, 14],
            polynomial.shift_coefficients(4).coefficients
        );
    }

    #[proptest]
    fn shifting_a_polynomial_means_prepending_zeros_to_its_coefficients(
        polynomial: Polynomial<BFieldElement>,
        #[strategy(0usize..30)] shift: usize,
    ) {
        let shifted_polynomial = polynomial.shift_coefficients(shift);
        let mut expected_coefficients = vec![BFieldElement::zero(); shift];
        expected_coefficients.extend(polynomial.coefficients);
        prop_assert_eq!(expected_coefficients, shifted_polynomial.coefficients);
    }

    #[proptest]
    fn any_polynomial_to_the_power_of_zero_is_one(poly: Polynomial<BFieldElement>) {
        let poly_to_the_zero = poly.mod_pow(0.into());
        prop_assert_eq!(Polynomial::one(), poly_to_the_zero);
    }

    #[proptest]
    fn any_polynomial_to_the_power_one_is_itself(poly: Polynomial<BFieldElement>) {
        let poly_to_the_one = poly.mod_pow(1.into());
        prop_assert_eq!(poly, poly_to_the_one);
    }

    #[proptest]
    fn polynomial_one_to_any_power_is_one(#[strategy(0u64..30)] exponent: u64) {
        let one_to_the_exponent = Polynomial::<BFieldElement>::one().mod_pow(exponent.into());
        prop_assert_eq!(Polynomial::one(), one_to_the_exponent);
    }

    #[test]
    fn mod_pow_test() {
        let polynomial = |cs: &[u64]| Polynomial::<BFieldElement>::from(cs);

        let pol = polynomial(&[0, 14, 0, 4, 0, 8, 0, 3]);
        let pol_squared = polynomial(&[0, 0, 196, 0, 112, 0, 240, 0, 148, 0, 88, 0, 48, 0, 9]);
        let pol_cubed = polynomial(&[
            0, 0, 0, 2744, 0, 2352, 0, 5376, 0, 4516, 0, 4080, 0, 2928, 0, 1466, 0, 684, 0, 216, 0,
            27,
        ]);

        assert_eq!(pol_squared, pol.mod_pow(2.into()));
        assert_eq!(pol_cubed, pol.mod_pow(3.into()));

        let parabola = polynomial(&[5, 41, 19]);
        let parabola_squared = polynomial(&[25, 410, 1871, 1558, 361]);
        assert_eq!(parabola_squared, parabola.mod_pow(2.into()));
    }

    #[proptest]
    fn mod_pow_arbitrary_test(
        poly: Polynomial<BFieldElement>,
        #[strategy(0u32..15)] exponent: u32,
    ) {
        let actual = poly.mod_pow(exponent.into());
        let fast_actual = poly.fast_mod_pow(exponent.into());
        let mut expected = Polynomial::one();
        for _ in 0..exponent {
            expected = expected.clone() * poly.clone();
        }

        prop_assert_eq!(expected.clone(), actual);
        prop_assert_eq!(expected, fast_actual);
    }

    #[proptest]
    fn polynomial_zero_is_neutral_element_for_addition(a: Polynomial<BFieldElement>) {
        prop_assert_eq!(a.clone() + Polynomial::zero(), a.clone());
        prop_assert_eq!(Polynomial::zero() + a.clone(), a);
    }

    #[proptest]
    fn polynomial_one_is_neutral_element_for_multiplication(a: Polynomial<BFieldElement>) {
        prop_assert_eq!(a.clone() * Polynomial::one(), a.clone());
        prop_assert_eq!(Polynomial::one() * a.clone(), a);
    }

    #[proptest]
    fn multiplication_by_zero_is_zero(a: Polynomial<BFieldElement>) {
        prop_assert_eq!(Polynomial::zero(), a.clone() * Polynomial::zero());
        prop_assert_eq!(Polynomial::zero(), Polynomial::zero() * a.clone());
    }

    #[proptest]
    fn polynomial_addition_is_commutative(
        a: Polynomial<BFieldElement>,
        b: Polynomial<BFieldElement>,
    ) {
        prop_assert_eq!(a.clone() + b.clone(), b + a);
    }

    #[proptest]
    fn polynomial_multiplication_is_commutative(
        a: Polynomial<BFieldElement>,
        b: Polynomial<BFieldElement>,
    ) {
        prop_assert_eq!(a.clone() * b.clone(), b * a);
    }

    #[proptest]
    fn polynomial_addition_is_associative(
        a: Polynomial<BFieldElement>,
        b: Polynomial<BFieldElement>,
        c: Polynomial<BFieldElement>,
    ) {
        prop_assert_eq!((a.clone() + b.clone()) + c.clone(), a + (b + c));
    }

    #[proptest]
    fn polynomial_multiplication_is_associative(
        a: Polynomial<BFieldElement>,
        b: Polynomial<BFieldElement>,
        c: Polynomial<BFieldElement>,
    ) {
        prop_assert_eq!((a.clone() * b.clone()) * c.clone(), a * (b * c));
    }

    #[proptest]
    fn polynomial_multiplication_is_distributive(
        a: Polynomial<BFieldElement>,
        b: Polynomial<BFieldElement>,
        c: Polynomial<BFieldElement>,
    ) {
        prop_assert_eq!(
            (a.clone() + b.clone()) * c.clone(),
            (a * c.clone()) + (b * c)
        );
    }

    #[proptest]
    fn polynomial_subtraction_of_self_is_zero(a: Polynomial<BFieldElement>) {
        prop_assert_eq!(Polynomial::zero(), a.clone() - a);
    }

    #[proptest]
    fn polynomial_division_by_self_is_one(#[filter(!#a.is_zero())] a: Polynomial<BFieldElement>) {
        prop_assert_eq!(Polynomial::one(), a.clone() / a);
    }

    #[proptest]
    fn polynomial_division_removes_common_factors(
        a: Polynomial<BFieldElement>,
        #[filter(!#b.is_zero())] b: Polynomial<BFieldElement>,
    ) {
        prop_assert_eq!(a.clone(), a * b.clone() / b);
    }

    #[proptest]
    fn polynomial_multiplication_raises_degree_at_maximum_to_sum_of_degrees(
        a: Polynomial<BFieldElement>,
        b: Polynomial<BFieldElement>,
    ) {
        let sum_of_degrees = (a.degree() + b.degree()).max(-1);
        prop_assert!((a * b).degree() <= sum_of_degrees);
    }

    #[test]
    fn leading_zeros_dont_affect_polynomial_division() {
        // This test was used to catch a bug where the polynomial division was wrong when the
        // divisor has a leading zero coefficient, i.e. when it was not normalized

        let polynomial = |cs: &[u64]| Polynomial::<BFieldElement>::from(cs);

        // x^3 - x + 1 / y = x
        let numerator = polynomial(&[1, BFieldElement::P - 1, 0, 1]);
        let numerator_with_leading_zero = polynomial(&[1, BFieldElement::P - 1, 0, 1, 0]);

        let divisor_normalized = polynomial(&[0, 1]);
        let divisor_not_normalized = polynomial(&[0, 1, 0]);
        let divisor_more_leading_zeros = polynomial(&[0, 1, 0, 0, 0, 0, 0, 0, 0]);

        let expected = polynomial(&[BFieldElement::P - 1, 0, 1]);

        // Verify that the divisor need not be normalized
        assert_eq!(expected, numerator.clone() / divisor_normalized.clone());
        assert_eq!(expected, numerator.clone() / divisor_not_normalized.clone());
        assert_eq!(expected, numerator / divisor_more_leading_zeros.clone());

        // Verify that numerator need not be normalized
        let res_numerator_not_normalized_0 =
            numerator_with_leading_zero.clone() / divisor_normalized;
        let res_numerator_not_normalized_1 =
            numerator_with_leading_zero.clone() / divisor_not_normalized;
        let res_numerator_not_normalized_2 =
            numerator_with_leading_zero / divisor_more_leading_zeros;
        assert_eq!(expected, res_numerator_not_normalized_0);
        assert_eq!(expected, res_numerator_not_normalized_1);
        assert_eq!(expected, res_numerator_not_normalized_2);
    }

    #[proptest]
    fn leading_coefficient_of_truncated_polynomial_is_same_as_original_leading_coefficient(
        poly: Polynomial<BFieldElement>,
        #[strategy(..50_usize)] truncation_point: usize,
    ) {
        let Some(lc) = poly.leading_coefficient() else {
            let reason = "test is only sensible if polynomial has a leading coefficient";
            return Err(TestCaseError::Reject(reason.into()));
        };
        let truncated_poly = poly.truncate(truncation_point);
        let Some(trunc_lc) = truncated_poly.leading_coefficient() else {
            let reason = "test is only sensible if truncated polynomial has a leading coefficient";
            return Err(TestCaseError::Reject(reason.into()));
        };
        prop_assert_eq!(lc, trunc_lc);
    }

    #[proptest]
    fn truncated_polynomial_is_of_degree_min_of_truncation_point_and_poly_degree(
        poly: Polynomial<BFieldElement>,
        #[strategy(..50_usize)] truncation_point: usize,
    ) {
        let expected_degree = poly.degree().min(truncation_point.try_into().unwrap());
        prop_assert_eq!(expected_degree, poly.truncate(truncation_point).degree());
    }

    #[proptest]
    fn truncating_zero_polynomial_gives_zero_polynomial(
        #[strategy(..50_usize)] truncation_point: usize,
    ) {
        let poly = Polynomial::<BFieldElement>::zero().truncate(truncation_point);
        prop_assert!(poly.is_zero());
    }

    #[proptest]
    fn truncation_negates_degree_shifting(
        #[strategy(0_usize..30)] shift: usize,
        #[strategy(..50_usize)] truncation_point: usize,
        #[filter(#poly.degree() >= #truncation_point as isize)] poly: Polynomial<BFieldElement>,
    ) {
        prop_assert_eq!(
            poly.truncate(truncation_point),
            poly.shift_coefficients(shift).truncate(truncation_point)
        );
    }

    #[proptest]
    fn zero_polynomial_mod_any_power_of_x_is_zero_polynomial(power: usize) {
        let must_be_zero = Polynomial::<BFieldElement>::zero().mod_x_to_the_n(power);
        prop_assert!(must_be_zero.is_zero());
    }

    #[proptest]
    fn polynomial_mod_some_power_of_x_results_in_polynomial_of_degree_one_less_than_power(
        #[filter(!#poly.is_zero())] poly: Polynomial<BFieldElement>,
        #[strategy(..=usize::try_from(#poly.degree()).unwrap())] power: usize,
    ) {
        let remainder = poly.mod_x_to_the_n(power);
        prop_assert_eq!(isize::try_from(power).unwrap() - 1, remainder.degree());
    }

    #[proptest]
    fn polynomial_mod_some_power_of_x_shares_low_degree_terms_coefficients_with_original_polynomial(
        #[filter(!#poly.is_zero())] poly: Polynomial<BFieldElement>,
        power: usize,
    ) {
        let remainder = poly.mod_x_to_the_n(power);
        let min_num_coefficients = poly.coefficients.len().min(remainder.coefficients.len());
        prop_assert_eq!(
            &poly.coefficients[..min_num_coefficients],
            &remainder.coefficients[..min_num_coefficients]
        );
    }

    #[proptest]
    fn fast_multiplication_by_zero_gives_zero(poly: Polynomial<BFieldElement>) {
        let product = poly.fast_multiply(&Polynomial::zero());
        prop_assert_eq!(Polynomial::zero(), product);
    }

    #[proptest]
    fn fast_multiplication_by_one_gives_self(poly: Polynomial<BFieldElement>) {
        let product = poly.fast_multiply(&Polynomial::one());
        prop_assert_eq!(poly, product);
    }

    #[proptest]
    fn fast_multiplication_is_commutative(
        a: Polynomial<BFieldElement>,
        b: Polynomial<BFieldElement>,
    ) {
        prop_assert_eq!(a.fast_multiply(&b), b.fast_multiply(&a));
    }

    #[proptest]
    fn fast_multiplication_and_normal_multiplication_are_equivalent(
        a: Polynomial<BFieldElement>,
        b: Polynomial<BFieldElement>,
    ) {
        let product = a.fast_multiply(&b);
        prop_assert_eq!(a * b, product);
    }

    #[proptest(cases = 50)]
    fn naive_zerofier_and_fast_zerofier_are_identical(
        #[any(size_range(..Polynomial::<BFieldElement>::FAST_ZEROFIER_CUTOFF_THRESHOLD * 2).lift())]
        roots: Vec<BFieldElement>,
    ) {
        let naive_zerofier = Polynomial::naive_zerofier(&roots);
        let fast_zerofier = Polynomial::fast_zerofier(&roots);
        prop_assert_eq!(naive_zerofier, fast_zerofier);
    }

    #[proptest(cases = 50)]
    fn smart_zerofier_and_fast_zerofier_are_identical(
        #[any(size_range(..Polynomial::<BFieldElement>::FAST_ZEROFIER_CUTOFF_THRESHOLD * 2).lift())]
        roots: Vec<BFieldElement>,
    ) {
        let smart_zerofier = Polynomial::smart_zerofier(&roots);
        let fast_zerofier = Polynomial::fast_zerofier(&roots);
        prop_assert_eq!(smart_zerofier, fast_zerofier);
    }

    #[proptest(cases = 50)]
    fn zerofier_and_naive_zerofier_are_identical(
        #[any(size_range(..Polynomial::<BFieldElement>::FAST_ZEROFIER_CUTOFF_THRESHOLD * 2).lift())]
        roots: Vec<BFieldElement>,
    ) {
        let zerofier = Polynomial::zerofier(&roots);
        let naive_zerofier = Polynomial::naive_zerofier(&roots);
        prop_assert_eq!(zerofier, naive_zerofier);
    }

    #[proptest(cases = 50)]
    fn zerofier_is_zero_only_on_domain(
        #[any(size_range(..1024).lift())] domain: Vec<BFieldElement>,
        #[filter(#out_of_domain_points.iter().all(|p| !#domain.contains(p)))]
        out_of_domain_points: Vec<BFieldElement>,
    ) {
        let zerofier = Polynomial::zerofier(&domain);
        for point in domain {
            prop_assert_eq!(BFieldElement::zero(), zerofier.evaluate(point));
        }
        for point in out_of_domain_points {
            prop_assert_ne!(BFieldElement::zero(), zerofier.evaluate(point));
        }
    }

    #[proptest]
    fn zerofier_has_leading_coefficient_one(domain: Vec<BFieldElement>) {
        let zerofier = Polynomial::zerofier(&domain);
        prop_assert_eq!(
            BFieldElement::one(),
            zerofier.leading_coefficient().unwrap()
        );
    }

    #[test]
    fn fast_evaluate_on_hardcoded_domain_and_polynomial() {
        let domain = bfe_array![6, 12];
        let x_to_the_5_plus_x_to_the_3 = Polynomial::new(bfe_vec![0, 0, 0, 1, 0, 1]);

        let manual_evaluations = domain.map(|x| x.mod_pow(5) + x.mod_pow(3)).to_vec();
        let fast_evaluations =
            x_to_the_5_plus_x_to_the_3.divide_and_conquer_batch_evaluate(&domain);
        assert_eq!(manual_evaluations, fast_evaluations);
    }

    #[proptest]
    fn slow_and_fast_polynomial_evaluation_are_equivalent(
        poly: Polynomial<BFieldElement>,
        #[any(size_range(..1024).lift())] domain: Vec<BFieldElement>,
    ) {
        let evaluations = domain.iter().map(|&x| poly.evaluate(x)).collect_vec();
        let fast_evaluations = poly.divide_and_conquer_batch_evaluate(&domain);
        prop_assert_eq!(evaluations, fast_evaluations);
    }

    #[test]
    #[should_panic(expected = "zero points")]
    fn lagrange_interpolation_through_no_points_is_impossible() {
        let _ = Polynomial::<BFieldElement>::lagrange_interpolate(&[], &[]);
    }

    #[proptest]
    fn interpolating_through_one_point_gives_constant_polynomial(
        x: BFieldElement,
        y: BFieldElement,
    ) {
        let interpolant = Polynomial::lagrange_interpolate(&[x], &[y]);
        let polynomial = Polynomial::from_constant(y);
        prop_assert_eq!(polynomial, interpolant);
    }

    #[test]
    #[should_panic(expected = "zero points")]
    fn fast_interpolation_through_no_points_is_impossible() {
        let _ = Polynomial::<BFieldElement>::fast_interpolate(&[], &[]);
    }

    #[proptest(cases = 10)]
    fn lagrange_and_fast_interpolation_are_identical(
        #[any(size_range(1..2048).lift())]
        #[filter(#domain.iter().all_unique())]
        domain: Vec<BFieldElement>,
        #[strategy(vec(arb(), #domain.len()))] values: Vec<BFieldElement>,
    ) {
        let lagrange_interpolant = Polynomial::lagrange_interpolate(&domain, &values);
        let fast_interpolant = Polynomial::fast_interpolate(&domain, &values);
        prop_assert_eq!(lagrange_interpolant, fast_interpolant);
    }

    #[test]
    fn fast_interpolation_through_a_single_point_succeeds() {
        let zero_arr = bfe_array![0];
        let _ = Polynomial::fast_interpolate(&zero_arr, &zero_arr);
    }

    #[proptest(cases = 20)]
    fn interpolation_then_evaluation_is_identity(
        #[any(size_range(1..2048).lift())]
        #[filter(#domain.iter().all_unique())]
        domain: Vec<BFieldElement>,
        #[strategy(vec(arb(), #domain.len()))] values: Vec<BFieldElement>,
    ) {
        let interpolant = Polynomial::fast_interpolate(&domain, &values);
        let evaluations = interpolant.divide_and_conquer_batch_evaluate(&domain);
        prop_assert_eq!(values, evaluations);
    }

    #[proptest(cases = 1)]
    fn fast_batch_interpolation_is_equivalent_to_fast_interpolation(
        #[any(size_range(1..2048).lift())]
        #[filter(#domain.iter().all_unique())]
        domain: Vec<BFieldElement>,
        #[strategy(vec(vec(arb(), #domain.len()), 0..10))] value_vecs: Vec<Vec<BFieldElement>>,
    ) {
        let root_order = domain.len().next_power_of_two();
        let root_of_unity = BFieldElement::primitive_root_of_unity(root_order as u64).unwrap();

        let interpolants = value_vecs
            .iter()
            .map(|values| Polynomial::fast_interpolate(&domain, values))
            .collect_vec();

        let batched_interpolants =
            Polynomial::batch_fast_interpolate(&domain, &value_vecs, root_of_unity, root_order);
        prop_assert_eq!(interpolants, batched_interpolants);
    }

    fn coset_domain_of_size_from_generator_with_offset(
        size: usize,
        generator: BFieldElement,
        offset: BFieldElement,
    ) -> Vec<BFieldElement> {
        let mut domain = vec![offset];
        for _ in 1..size {
            domain.push(domain.last().copied().unwrap() * generator);
        }
        domain
    }

    #[proptest]
    fn fast_coset_evaluation_and_fast_evaluation_on_coset_are_identical(
        polynomial: Polynomial<BFieldElement>,
        offset: BFieldElement,
        #[strategy(0..8usize)]
        #[map(|x: usize| 1 << x)]
        // due to current limitation in `Polynomial::fast_coset_evaluate`
        #[filter((#root_order as isize) > #polynomial.degree())]
        root_order: usize,
    ) {
        let root_of_unity = BFieldElement::primitive_root_of_unity(root_order as u64).unwrap();
        let domain =
            coset_domain_of_size_from_generator_with_offset(root_order, root_of_unity, offset);

        let fast_values = polynomial.divide_and_conquer_batch_evaluate(&domain);
        let fast_coset_values = polynomial.fast_coset_evaluate(offset, root_of_unity, root_order);
        prop_assert_eq!(fast_values, fast_coset_values);
    }

    #[proptest]
    fn fast_coset_interpolation_and_and_fast_interpolation_on_coset_are_identical(
        #[filter(!#offset.is_zero())] offset: BFieldElement,
        #[strategy(1..8usize)]
        #[map(|x: usize| 1 << x)]
        root_order: usize,
        #[strategy(vec(arb(), #root_order))] values: Vec<BFieldElement>,
    ) {
        let root_of_unity = BFieldElement::primitive_root_of_unity(root_order as u64).unwrap();
        let domain =
            coset_domain_of_size_from_generator_with_offset(root_order, root_of_unity, offset);

        let fast_interpolant = Polynomial::fast_interpolate(&domain, &values);
        let fast_coset_interpolant =
            Polynomial::fast_coset_interpolate(offset, root_of_unity, &values);
        prop_assert_eq!(fast_interpolant, fast_coset_interpolant);
    }

    #[proptest]
    fn naive_division_gives_quotient_and_remainder_with_expected_properties(
        a: Polynomial<BFieldElement>,
        #[filter(!#b.is_zero())] b: Polynomial<BFieldElement>,
    ) {
        let (quot, rem) = a.naive_divide(&b);
        prop_assert!(rem.degree() < b.degree());
        prop_assert_eq!(a, quot * b + rem);
    }

    #[proptest]
    fn clean_naive_division_gives_quotient_and_remainder_with_expected_properties(
        #[filter(!#a_roots.is_empty())] a_roots: Vec<BFieldElement>,
        #[strategy(vec(0..#a_roots.len(), 1..=#a_roots.len()))]
        #[filter(#b_root_indices.iter().all_unique())]
        b_root_indices: Vec<usize>,
    ) {
        let b_roots = b_root_indices.into_iter().map(|i| a_roots[i]).collect_vec();
        let a = Polynomial::zerofier(&a_roots);
        let b = Polynomial::zerofier(&b_roots);
        let (quot, rem) = a.naive_divide(&b);
        prop_assert!(rem.is_zero());
        prop_assert_eq!(a, quot * b);
    }

    #[proptest]
    fn naive_division_and_fast_division_are_equivalent(
        a: Polynomial<BFieldElement>,
        #[filter(!#b.is_zero())] b: Polynomial<BFieldElement>,
    ) {
        let (quotient, _remainder) = a.fast_divide(&b);
        prop_assert_eq!(a / b, quotient);
    }

    #[proptest]
    fn clean_division_agrees_with_divide_on_clean_division(
        #[strategy(arb())] a: Polynomial<BFieldElement>,
        #[strategy(arb())]
        #[filter(!#b.is_zero())]
        b: Polynomial<BFieldElement>,
    ) {
        let product = a.clone() * b.clone();
        let (naive_quotient, naive_remainder) = product.naive_divide(&b);
        let clean_quotient = product.clone().clean_divide(b.clone());
        let err = format!("{product} / {b} == {naive_quotient} != {clean_quotient}");
        prop_assert_eq!(naive_quotient, clean_quotient, "{}", err);
        prop_assert_eq!(Polynomial::<BFieldElement>::zero(), naive_remainder);
    }

    #[proptest]
    fn clean_division_agrees_with_division_if_divisor_has_only_0_as_root(
        #[strategy(arb())] mut dividend_roots: Vec<BFieldElement>,
    ) {
        dividend_roots.push(bfe!(0));
        let dividend = Polynomial::zerofier(&dividend_roots);
        let divisor = Polynomial::zerofier(&[bfe!(0)]);

        let (naive_quotient, remainder) = dividend.naive_divide(&divisor);
        let clean_quotient = dividend.clean_divide(divisor);
        prop_assert_eq!(naive_quotient, clean_quotient);
        prop_assert_eq!(Polynomial::<BFieldElement>::zero(), remainder);
    }

    #[proptest]
    fn clean_division_agrees_with_division_if_divisor_has_only_0_as_multiple_root(
        #[strategy(arb())] mut dividend_roots: Vec<BFieldElement>,
        #[strategy(0_usize..300)] num_roots: usize,
    ) {
        let multiple_roots = bfe_vec![0; num_roots];
        let divisor = Polynomial::zerofier(&multiple_roots);
        dividend_roots.extend(multiple_roots);
        let dividend = Polynomial::zerofier(&dividend_roots);

        let (naive_quotient, remainder) = dividend.naive_divide(&divisor);
        let clean_quotient = dividend.clean_divide(divisor);
        prop_assert_eq!(naive_quotient, clean_quotient);
        prop_assert_eq!(Polynomial::<BFieldElement>::zero(), remainder);
    }

    #[proptest]
    fn clean_division_agrees_with_division_if_divisor_has_0_as_root(
        #[strategy(arb())] mut dividend_roots: Vec<BFieldElement>,
        #[strategy(vec(0..#dividend_roots.len(), 0..=#dividend_roots.len()))]
        #[filter(#divisor_root_indices.iter().all_unique())]
        divisor_root_indices: Vec<usize>,
    ) {
        // ensure clean division: make divisor's roots a subset of dividend's roots
        let mut divisor_roots = divisor_root_indices
            .into_iter()
            .map(|i| dividend_roots[i])
            .collect_vec();

        // ensure clean division: make 0 a root of both dividend and divisor
        dividend_roots.push(bfe!(0));
        divisor_roots.push(bfe!(0));

        let dividend = Polynomial::zerofier(&dividend_roots);
        let divisor = Polynomial::zerofier(&divisor_roots);
        let quotient = dividend.clone().clean_divide(divisor.clone());
        prop_assert_eq!(dividend / divisor, quotient);
    }

    #[proptest]
    fn clean_division_agrees_with_division_if_divisor_has_0_through_9_as_roots(
        #[strategy(arb())] additional_dividend_roots: Vec<BFieldElement>,
    ) {
        let divisor_roots = (0..10).map(BFieldElement::new).collect_vec();
        let divisor = Polynomial::zerofier(&divisor_roots);
        let dividend_roots = [additional_dividend_roots, divisor_roots].concat();
        let dividend = Polynomial::zerofier(&dividend_roots);
        dbg!(dividend.to_string());
        dbg!(divisor.to_string());
        let quotient = dividend.clone().clean_divide(divisor.clone());
        prop_assert_eq!(dividend / divisor, quotient);
    }

    #[proptest]
    fn clean_division_gives_quotient_and_remainder_with_expected_properties(
        #[filter(!#a_roots.is_empty())] a_roots: Vec<BFieldElement>,
        #[strategy(vec(0..#a_roots.len(), 1..=#a_roots.len()))]
        #[filter(#b_root_indices.iter().all_unique())]
        b_root_indices: Vec<usize>,
    ) {
        let b_roots = b_root_indices.into_iter().map(|i| a_roots[i]).collect_vec();
        let a = Polynomial::zerofier(&a_roots);
        let b = Polynomial::zerofier(&b_roots);
        let quotient = a.clone().clean_divide(b.clone());
        prop_assert_eq!(a, quotient * b);
    }

    #[proptest]
    fn dividing_constant_polynomials_is_equivalent_to_dividing_constants(
        a: BFieldElement,
        #[filter(!#b.is_zero())] b: BFieldElement,
    ) {
        let a_poly = Polynomial::from_constant(a);
        let b_poly = Polynomial::from_constant(b);
        let expected_quotient = Polynomial::from_constant(a / b);
        prop_assert_eq!(expected_quotient, a_poly / b_poly);
    }

    #[proptest]
    fn dividing_any_polynomial_by_a_constant_polynomial_results_in_remainder_zero(
        a: Polynomial<BFieldElement>,
        #[filter(!#b.is_zero())] b: BFieldElement,
    ) {
        let b_poly = Polynomial::from_constant(b);
        let (_, remainder) = a.naive_divide(&b_poly);
        prop_assert_eq!(Polynomial::zero(), remainder);
    }

    #[test]
    fn polynomial_division_by_and_with_shah_polynomial() {
        let polynomial = |cs: &[u64]| Polynomial::<BFieldElement>::from(cs);

        let shah = XFieldElement::shah_polynomial();
        let x_to_the_3 = polynomial(&[1]).shift_coefficients(3);
        let (shah_div_x_to_the_3, shah_mod_x_to_the_3) = shah.naive_divide(&x_to_the_3);
        assert_eq!(polynomial(&[1]), shah_div_x_to_the_3);
        assert_eq!(polynomial(&[1, BFieldElement::P - 1]), shah_mod_x_to_the_3);

        let x_to_the_6 = polynomial(&[1]).shift_coefficients(6);
        let (x_to_the_6_div_shah, x_to_the_6_mod_shah) = x_to_the_6.naive_divide(&shah);

        // x^3 + x - 1
        let expected_quot = polynomial(&[BFieldElement::P - 1, 1, 0, 1]);
        assert_eq!(expected_quot, x_to_the_6_div_shah);

        // x^2 - 2x + 1
        let expected_rem = polynomial(&[1, BFieldElement::P - 2, 1]);
        assert_eq!(expected_rem, x_to_the_6_mod_shah);
    }

    #[test]
    fn xgcd_does_not_panic_on_input_zero() {
        let zero = Polynomial::<BFieldElement>::zero;
        let (gcd, a, b) = Polynomial::xgcd(zero(), zero());
        assert_eq!(zero(), gcd);
        println!("a = {a}");
        println!("b = {b}");
    }

    #[proptest]
    fn xgcd_b_field_pol_test(x: Polynomial<BFieldElement>, y: Polynomial<BFieldElement>) {
        let (gcd, a, b) = Polynomial::xgcd(x.clone(), y.clone());
        // Bezout relation
        prop_assert_eq!(gcd, a * x + b * y);
    }

    #[proptest]
    fn xgcd_x_field_pol_test(x: Polynomial<XFieldElement>, y: Polynomial<XFieldElement>) {
        let (gcd, a, b) = Polynomial::xgcd(x.clone(), y.clone());
        // Bezout relation
        prop_assert_eq!(gcd, a * x + b * y);
    }

    #[proptest]
    fn add_assign_is_equivalent_to_adding_and_assigning(
        a: Polynomial<BFieldElement>,
        b: Polynomial<BFieldElement>,
    ) {
        let mut c = a.clone();
        c += b.clone();
        prop_assert_eq!(a + b, c);
    }

    #[test]
    fn only_monic_polynomial_of_degree_1_is_x() {
        let polynomial = |cs: &[u64]| Polynomial::<BFieldElement>::from(cs);

        assert!(polynomial(&[0, 1]).is_x());
        assert!(polynomial(&[0, 1, 0]).is_x());
        assert!(polynomial(&[0, 1, 0, 0]).is_x());

        assert!(!polynomial(&[]).is_x());
        assert!(!polynomial(&[0]).is_x());
        assert!(!polynomial(&[1]).is_x());
        assert!(!polynomial(&[1, 0]).is_x());
        assert!(!polynomial(&[0, 2]).is_x());
        assert!(!polynomial(&[0, 0, 1]).is_x());
    }

    #[test]
    fn hardcoded_polynomial_squaring() {
        let polynomial = |cs: &[u64]| Polynomial::<BFieldElement>::from(cs);

        assert_eq!(Polynomial::zero(), polynomial(&[]).square());

        let x_plus_1 = polynomial(&[1, 1]);
        assert_eq!(polynomial(&[1, 2, 1]), x_plus_1.square());

        let x_to_the_15 = polynomial(&[1]).shift_coefficients(15);
        let x_to_the_30 = polynomial(&[1]).shift_coefficients(30);
        assert_eq!(x_to_the_30, x_to_the_15.square());

        let some_poly = polynomial(&[14, 1, 3, 4]);
        assert_eq!(
            polynomial(&[196, 28, 85, 118, 17, 24, 16]),
            some_poly.square()
        );
    }

    #[proptest]
    fn polynomial_squaring_is_equivalent_to_multiplication_with_self(
        poly: Polynomial<BFieldElement>,
    ) {
        prop_assert_eq!(poly.clone() * poly.clone(), poly.square());
    }

    #[proptest]
    fn slow_and_normal_squaring_are_equivalent(poly: Polynomial<BFieldElement>) {
        prop_assert_eq!(poly.slow_square(), poly.square());
    }

    #[proptest]
    fn normal_and_fast_squaring_are_equivalent(poly: Polynomial<BFieldElement>) {
        prop_assert_eq!(poly.square(), poly.fast_square());
    }

    #[test]
    fn constant_zero_eq_constant_zero() {
        let zero_polynomial1 = Polynomial::<BFieldElement>::zero();
        let zero_polynomial2 = Polynomial::<BFieldElement>::zero();

        assert_eq!(zero_polynomial1, zero_polynomial2)
    }

    #[test]
    fn zero_polynomial_is_zero() {
        assert!(Polynomial::<BFieldElement>::zero().is_zero());
        assert!(Polynomial::<XFieldElement>::zero().is_zero());
    }

    #[proptest]
    fn zero_polynomial_is_zero_independent_of_spurious_leading_zeros(
        #[strategy(..500usize)] num_zeros: usize,
    ) {
        let coefficients = vec![0; num_zeros];
        prop_assert_eq!(
            Polynomial::zero(),
            Polynomial::<BFieldElement>::from(coefficients)
        );
    }

    #[proptest]
    fn no_constant_polynomial_with_non_zero_coefficient_is_zero(
        #[filter(!#constant.is_zero())] constant: BFieldElement,
    ) {
        let constant_polynomial = Polynomial::from_constant(constant);
        prop_assert!(!constant_polynomial.is_zero());
    }

    #[test]
    fn constant_one_eq_constant_one() {
        let one_polynomial1 = Polynomial::<BFieldElement>::one();
        let one_polynomial2 = Polynomial::<BFieldElement>::one();

        assert_eq!(one_polynomial1, one_polynomial2)
    }

    #[test]
    fn one_polynomial_is_one() {
        assert!(Polynomial::<BFieldElement>::one().is_one());
        assert!(Polynomial::<XFieldElement>::one().is_one());
    }

    #[proptest]
    fn one_polynomial_is_one_independent_of_spurious_leading_zeros(
        #[strategy(..500usize)] num_leading_zeros: usize,
    ) {
        let spurious_leading_zeros = vec![0; num_leading_zeros];
        let mut coefficients = vec![1];
        coefficients.extend(spurious_leading_zeros);
        prop_assert_eq!(
            Polynomial::one(),
            Polynomial::<BFieldElement>::from(coefficients)
        );
    }

    #[proptest]
    fn no_constant_polynomial_with_non_one_coefficient_is_one(
        #[filter(!#constant.is_one())] constant: BFieldElement,
    ) {
        let constant_polynomial = Polynomial::from_constant(constant);
        prop_assert!(!constant_polynomial.is_one());
    }

    #[test]
    fn formal_derivative_of_zero_is_zero() {
        assert!(Polynomial::<BFieldElement>::zero()
            .formal_derivative()
            .is_zero());
        assert!(Polynomial::<XFieldElement>::zero()
            .formal_derivative()
            .is_zero());
    }

    #[proptest]
    fn formal_derivative_of_constant_polynomial_is_zero(constant: BFieldElement) {
        let formal_derivative = Polynomial::from_constant(constant).formal_derivative();
        prop_assert!(formal_derivative.is_zero());
    }

    #[proptest]
    fn formal_derivative_of_non_zero_polynomial_is_of_degree_one_less_than_the_polynomial(
        #[filter(!#poly.is_zero())] poly: Polynomial<BFieldElement>,
    ) {
        prop_assert_eq!(poly.degree() - 1, poly.formal_derivative().degree());
    }

    #[proptest]
    fn formal_derivative_of_product_adheres_to_the_leibniz_product_rule(
        a: Polynomial<BFieldElement>,
        b: Polynomial<BFieldElement>,
    ) {
        let product_formal_derivative = (a.clone() * b.clone()).formal_derivative();
        let product_rule = a.formal_derivative() * b.clone() + a * b.formal_derivative();
        prop_assert_eq!(product_rule, product_formal_derivative);
    }

    #[test]
    fn zero_is_zero() {
        let f = Polynomial::new(vec![BFieldElement::new(0)]);
        assert!(f.is_zero());
    }

    #[proptest]
    fn formal_power_series_inverse_newton(
        #[strategy(2usize..20)] precision: usize,
        #[filter(!#f.coefficients.is_empty())]
        #[filter(!#f.coefficients[0].is_zero())]
        #[filter(#precision > 1 + #f.degree() as usize)]
        f: Polynomial<BFieldElement>,
    ) {
        let g = f.formal_power_series_inverse_newton(precision);
        let mut coefficients = vec![BFieldElement::new(0); precision + 1];
        coefficients[precision] = BFieldElement::new(1);
        let xn = Polynomial::new(coefficients);
        let (_quotient, remainder) = g.multiply(&f).divide(&xn);
        prop_assert!(remainder.is_one());
    }

    #[test]
    fn formal_power_series_inverse_newton_concrete() {
        let f = Polynomial::<BFieldElement>::new(vec![
            BFieldElement::new(3618372803227210457),
            BFieldElement::new(14620511201754172786),
            BFieldElement::new(2577803283145951105),
            BFieldElement::new(1723541458268087404),
            BFieldElement::new(4119508755381840018),
            BFieldElement::new(8592072587377832596),
            BFieldElement::new(236223201225),
        ]);
        let precision = 8;

        let g = f.formal_power_series_inverse_newton(precision);
        let mut coefficients = vec![BFieldElement::new(0); precision + 1];
        coefficients[precision] = BFieldElement::new(1);
        let xn = Polynomial::new(coefficients);
        let (_quotient, remainder) = g.multiply(&f).divide(&xn);
        assert!(remainder.is_one());
    }

    #[proptest]
    fn formal_power_series_inverse_minimal(
        #[strategy(2usize..20)] precision: usize,
        #[filter(!#f.coefficients.is_empty())]
        #[filter(!#f.coefficients[0].is_zero())]
        #[filter(#precision > 1 + #f.degree() as usize)]
        f: Polynomial<BFieldElement>,
    ) {
        let g = f.formal_power_series_inverse_minimal(precision);
        let mut coefficients = vec![BFieldElement::new(0); precision + 1];
        coefficients[precision] = BFieldElement::new(1);
        let xn = Polynomial::new(coefficients);
        let (_quotient, remainder) = g.multiply(&f).divide(&xn);

        // inverse in formal power series ring
        prop_assert!(remainder.is_one());

        // minimal?
        prop_assert!(g.degree() <= precision as isize);
    }

    #[proptest]
    fn structured_multiple_is_multiple(
        #[filter(#coefficients.iter().any(|c|!c.is_zero()))]
        #[strategy(vec(arb(), 1..30))]
        coefficients: Vec<BFieldElement>,
    ) {
        let polynomial = Polynomial::<BFieldElement>::new(coefficients);
        let multiple = polynomial.structured_multiple();
        let (_quotient, remainder) = multiple.divide(&polynomial);
        prop_assert!(remainder.is_zero());
    }

    #[proptest]
    fn structured_multiple_generates_structure(
        #[filter(#coefficients.iter().filter(|c|!c.is_zero()).count() >= 3)]
        #[strategy(vec(arb(), 1..30))]
        coefficients: Vec<BFieldElement>,
    ) {
        let polynomial = Polynomial::<BFieldElement>::new(coefficients);
        let n = polynomial.degree() as usize;
        let structured_multiple = polynomial.structured_multiple();
        assert!(structured_multiple.degree() as usize <= 2 * n);

        let x2n = Polynomial::new(
            [
                vec![BFieldElement::new(0); 2 * n],
                vec![BFieldElement::new(1); 1],
            ]
            .concat(),
        );
        let remainder = structured_multiple.reduce(&x2n);
        assert_eq!(remainder.degree() as usize, n - 1);
        assert_eq!(
            (structured_multiple.clone() - remainder.clone())
                .reverse()
                .degree() as usize,
            0
        );
        assert_eq!(
            *(structured_multiple.clone() - remainder)
                .coefficients
                .last()
                .unwrap(),
            BFieldElement::new(1)
        );
    }

    #[test]
    fn structured_multiple_generates_structure_concrete() {
        let polynomial = Polynomial::new(
            [884763262770, 0, 51539607540, 14563891882495327437]
                .into_iter()
                .map(BFieldElement::new)
                .collect_vec(),
        );
        let n = polynomial.degree() as usize;
        let structured_multiple = polynomial.structured_multiple();
        assert!(structured_multiple.degree() as usize <= 2 * n);

        let x2n = Polynomial::new(
            [
                vec![BFieldElement::new(0); 2 * n],
                vec![BFieldElement::new(1); 1],
            ]
            .concat(),
        );
        let (_quotient, remainder) = structured_multiple.divide(&x2n);
        assert_eq!(remainder.degree() as usize, n - 1);
        assert_eq!(
            (structured_multiple.clone() - remainder.clone())
                .reverse()
                .degree() as usize,
            0
        );
        assert_eq!(
            *(structured_multiple.clone() - remainder)
                .coefficients
                .last()
                .unwrap(),
            BFieldElement::new(1)
        );
    }

    #[proptest]
    fn structured_multiple_of_degree_is_multiple(
        #[strategy(2usize..100)] n: usize,
        #[filter(#coefficients.iter().any(|c|!c.is_zero()))]
        #[strategy(vec(arb(), 1..usize::min(30, #n)))]
        coefficients: Vec<BFieldElement>,
    ) {
        let polynomial = Polynomial::<BFieldElement>::new(coefficients);
        let multiple = polynomial.structured_multiple_of_degree(n);
        let remainder = multiple.reduce(&polynomial);
        prop_assert!(remainder.is_zero());
    }

    #[proptest]
    fn structured_multiple_of_degree_generates_structure(
        #[strategy(4usize..100)] n: usize,
        #[strategy(vec(arb(), 3..usize::min(30, #n)))] mut coefficients: Vec<BFieldElement>,
    ) {
        *coefficients.last_mut().unwrap() = BFieldElement::new(1);
        let polynomial = Polynomial::<BFieldElement>::new(coefficients);
        let structured_multiple = polynomial.structured_multiple_of_degree(n);

        let xn = Polynomial::new(
            [
                vec![BFieldElement::new(0); n],
                vec![BFieldElement::new(1); 1],
            ]
            .concat(),
        );
        let remainder = structured_multiple.reduce(&xn);
        assert_eq!(
            (structured_multiple.clone() - remainder.clone())
                .reverse()
                .degree() as usize,
            0
        );
        assert_eq!(
            *(structured_multiple.clone() - remainder)
                .coefficients
                .last()
                .unwrap(),
            BFieldElement::new(1)
        );
    }

    #[proptest]
    fn structured_multiple_of_degree_has_given_degree(
        #[strategy(2usize..100)] n: usize,
        #[filter(#coefficients.iter().any(|c|!c.is_zero()))]
        #[strategy(vec(arb(), 1..usize::min(30, #n)))]
        coefficients: Vec<BFieldElement>,
    ) {
        let polynomial = Polynomial::<BFieldElement>::new(coefficients);
        let multiple = polynomial.structured_multiple_of_degree(n);
        prop_assert_eq!(
            multiple.degree() as usize,
            n,
            "polynomial: {} whereas multiple {}",
            polynomial,
            multiple
        );
    }

    #[proptest]
    fn reverse_polynomial_with_nonzero_constant_term_twice_gives_original_back(
        f: Polynomial<BFieldElement>,
    ) {
        let fx_plus_1 = f.shift_coefficients(1)
            + Polynomial::<BFieldElement>::from_constant(BFieldElement::new(1));
        prop_assert_eq!(fx_plus_1.clone(), fx_plus_1.reverse().reverse());
    }

    #[proptest]
    fn reverse_polynomial_with_zero_constant_term_twice_gives_shift_back(
        #[filter(!#f.is_zero())] f: Polynomial<BFieldElement>,
    ) {
        let fx_plus_1 = f.shift_coefficients(1);
        prop_assert_ne!(fx_plus_1.clone(), fx_plus_1.reverse().reverse());
        prop_assert_eq!(
            fx_plus_1.clone(),
            fx_plus_1.reverse().reverse().shift_coefficients(1)
        );
    }

    #[proptest]
    fn reduce_by_structured_modulus_and_reduce_agree(
        #[strategy(1usize..10)] n: usize,
        #[strategy(1usize..10)] m: usize,
        #[strategy(vec(arb(), #m))] b_coefficients: Vec<BFieldElement>,
        #[strategy(1usize..100)] _deg_a: usize,
        #[strategy(vec(arb(), #_deg_a + 1))] _a_coefficients: Vec<BFieldElement>,
        #[strategy(Just(Polynomial::new(#_a_coefficients)))] a: Polynomial<BFieldElement>,
    ) {
        let mut full_modulus_coefficients = b_coefficients.clone();
        full_modulus_coefficients.resize(m + n + 1, BFieldElement::from(0));
        *full_modulus_coefficients.last_mut().unwrap() = BFieldElement::from(1);
        let full_modulus = Polynomial::new(full_modulus_coefficients);

        let long_remainder = a.reduce(&full_modulus);
        let structured_remainder = a.reduce_by_structured_modulus(&full_modulus);

        prop_assert_eq!(long_remainder, structured_remainder);
    }

    #[test]
    fn reduce_by_structured_modulus_and_reduce_agree_concrete() {
        let a = Polynomial::new(
            [1, 0, 0, 3, 4, 3, 1, 5, 1, 0, 1, 2, 9, 2, 0, 3, 1]
                .into_iter()
                .map(BFieldElement::new)
                .collect_vec(),
        );
        let mut full_modulus_coefficients =
            [5, 6, 3].into_iter().map(BFieldElement::new).collect_vec();
        let m = full_modulus_coefficients.len();
        let n = 2;
        full_modulus_coefficients.resize(m + n + 1, BFieldElement::from(0));
        *full_modulus_coefficients.last_mut().unwrap() = BFieldElement::from(1);
        let full_modulus = Polynomial::new(full_modulus_coefficients);

        let long_remainder = a.reduce(&full_modulus);
        let structured_remainder = a.reduce_by_structured_modulus(&full_modulus);

        assert_eq!(
            long_remainder, structured_remainder,
            "naive: {}\nstructured: {}",
            long_remainder, structured_remainder
        );
    }

    #[proptest]
    fn reduce_by_ntt_friendly_modulus_and_reduce_agree(
        #[strategy(1usize..10)] m: usize,
        #[strategy(vec(arb(), #m))] b_coefficients: Vec<BFieldElement>,
        #[strategy(1usize..100)] _deg_a: usize,
        #[strategy(vec(arb(), #_deg_a + 1))] _a_coefficients: Vec<BFieldElement>,
        #[strategy(Just(Polynomial::new(#_a_coefficients)))] a: Polynomial<BFieldElement>,
    ) {
        if !b_coefficients.iter().all(|c| c.is_zero()) {
            let n = (b_coefficients.len() + 1).next_power_of_two();
            let mut full_modulus_coefficients = b_coefficients.clone();
            full_modulus_coefficients.resize(n + 1, BFieldElement::from(0));
            *full_modulus_coefficients.last_mut().unwrap() = BFieldElement::from(1);
            let full_modulus = Polynomial::new(full_modulus_coefficients);

            let long_remainder = a.reduce(&full_modulus);

            let mut shift_ntt = b_coefficients.clone();
            shift_ntt.resize(n, BFieldElement::from(0));
            ntt(
                &mut shift_ntt,
                BFieldElement::primitive_root_of_unity(n as u64).unwrap(),
                n.ilog2(),
            );
            let structured_remainder = a.reduce_by_ntt_friendly_modulus(&shift_ntt, m);

            prop_assert_eq!(long_remainder, structured_remainder);
        }
    }

    #[test]
    fn reduce_by_ntt_friendly_modulus_and_reduce_agree_concrete() {
        let m = 1;
        let a_coefficients = [0, 0, 532575944580]
            .into_iter()
            .map(BFieldElement::new)
            .collect_vec();
        let a = Polynomial::new(a_coefficients);
        let b_coefficients = vec![BFieldElement::new(944892804900)];
        let n = (b_coefficients.len() + 1).next_power_of_two();
        let mut full_modulus_coefficients = b_coefficients.clone();
        full_modulus_coefficients.resize(n + 1, BFieldElement::from(0));
        *full_modulus_coefficients.last_mut().unwrap() = BFieldElement::from(1);
        let full_modulus = Polynomial::new(full_modulus_coefficients);

        let long_remainder = a.reduce(&full_modulus);

        let mut shift_ntt = b_coefficients.clone();
        shift_ntt.resize(n, BFieldElement::from(0));
        ntt(
            &mut shift_ntt,
            BFieldElement::primitive_root_of_unity(n as u64).unwrap(),
            n.ilog2(),
        );
        let structured_remainder = a.reduce_by_ntt_friendly_modulus(&shift_ntt, m);

        assert_eq!(
            long_remainder, structured_remainder,
            "full modulus: {}",
            full_modulus
        );
    }

    #[proptest]
    fn fast_reduce_agrees_with_reduce(
        #[filter(!#a.is_zero())] a: Polynomial<BFieldElement>,
        #[filter(!#b.is_zero())] b: Polynomial<BFieldElement>,
    ) {
        let multiple = b.structured_multiple();
        let long_remainder = a.reduce(&b);
        let fast_remainder = a.reduce_with_structured_multiple(&b, &multiple);
        prop_assert_eq!(long_remainder, fast_remainder);
    }

    #[test]
    fn fast_reduce_agrees_with_reduce_concrete() {
        let a = Polynomial::new(
            vec![1, 2, 3, 3, 4, 5, 5, 4, 3, 5, 6]
                .into_iter()
                .map(BFieldElement::new)
                .collect_vec(),
        );
        let b = Polynomial::new(vec![1, 3].into_iter().map(BFieldElement::new).collect_vec());
        let multiple = b.structured_multiple();

        let long_remainder = a.reduce(&b);
        let fast_remainder = a.reduce_with_structured_multiple(&b, &multiple);
        assert_eq!(long_remainder, fast_remainder);
    }

    #[proptest]
    fn preprocessed_reduce_agrees_with_reduce(
        #[filter(!#a.is_zero())] a: Polynomial<BFieldElement>,
        #[filter(!#b.is_zero())] b: Polynomial<BFieldElement>,
    ) {
        let standard_remainder = a.reduce(&b);
        let preprocessed_remainder = a.fast_reduce(&b);
        prop_assert_eq!(standard_remainder, preprocessed_remainder);
    }

    #[test]
    fn preprocessed_reduce_agrees_with_reduce_on_huge_polynomials() {
        // This test could be made into a proptest but to make test time
        // manageable we would have to set cases=1. And then what's the point?
        let mut rng = thread_rng();
        let deg_a = rng.gen_range((1 << 15)..(1 << 16));
        let a_coefficients = (0..=deg_a)
            .map(|_| rng.gen::<BFieldElement>())
            .collect_vec();
        let deg_b = rng.gen_range((1 << 5)..(1 << 6));
        let b_coefficients = (0..=deg_b)
            .map(|_| rng.gen::<BFieldElement>())
            .collect_vec();
        let a = Polynomial::new(a_coefficients);
        let b = Polynomial::new(b_coefficients);
        let standard_remainder = a.reduce(&b);
        let preprocessed_remainder = a.fast_reduce(&b);
        assert_eq!(standard_remainder, preprocessed_remainder);
    }

    #[proptest]
    fn batch_evaluate_methods_are_equivalent(
        #[strategy(vec(arb(), (1<<10)..(1<<11)))] coefficients: Vec<BFieldElement>,
        #[strategy(vec(arb(), (1<<5)..(1<<7)))] domain: Vec<BFieldElement>,
    ) {
        let polynomial = Polynomial::new(coefficients);
        let evaluations_iterative = polynomial.iterative_batch_evaluate(&domain);
        let evaluations_fast = polynomial.divide_and_conquer_batch_evaluate(&domain);
        let evaluations_reduce_then = polynomial.reduce_then_batch_evaluate(&domain);

        prop_assert_eq!(evaluations_iterative.clone(), evaluations_fast);
        prop_assert_eq!(evaluations_iterative, evaluations_reduce_then);
    }
}
