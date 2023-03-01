use std::cmp::Ordering;
use std::fmt::{self, Debug, Formatter, Write};
use std::ops::{Add, AddAssign};

use ecow::{eco_format, EcoString, EcoVec};

use super::{ops, Args, Func, Value, Vm};
use crate::diag::{bail, At, SourceResult, StrResult};

/// Create a new [`Array`] from values.
#[macro_export]
#[doc(hidden)]
macro_rules! __array {
    ($value:expr; $count:expr) => {
        $crate::model::Array::from_vec($crate::model::eco_vec![$value.into(); $count])
    };

    ($($value:expr),* $(,)?) => {
        $crate::model::Array::from_vec($crate::model::eco_vec![$($value.into()),*])
    };
}

#[doc(inline)]
pub use crate::__array as array;
#[doc(hidden)]
pub use ecow::eco_vec;

/// A reference counted array with value semantics.
#[derive(Default, Clone, PartialEq, Hash)]
pub struct Array(EcoVec<Value>);

impl Array {
    /// Create a new, empty array.
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a new array from an eco vector of values.
    pub fn from_vec(vec: EcoVec<Value>) -> Self {
        Self(vec)
    }

    /// The length of the array.
    pub fn len(&self) -> i64 {
        self.0.len() as i64
    }

    /// The first value in the array.
    pub fn first(&self) -> StrResult<&Value> {
        self.0.first().ok_or_else(array_is_empty)
    }

    /// Mutably borrow the first value in the array.
    pub fn first_mut(&mut self) -> StrResult<&mut Value> {
        self.0.make_mut().first_mut().ok_or_else(array_is_empty)
    }

    /// The last value in the array.
    pub fn last(&self) -> StrResult<&Value> {
        self.0.last().ok_or_else(array_is_empty)
    }

    /// Mutably borrow the last value in the array.
    pub fn last_mut(&mut self) -> StrResult<&mut Value> {
        self.0.make_mut().last_mut().ok_or_else(array_is_empty)
    }

    /// Borrow the value at the given index.
    pub fn at(&self, index: i64) -> StrResult<&Value> {
        self.locate(index)
            .and_then(|i| self.0.get(i))
            .ok_or_else(|| out_of_bounds(index, self.len()))
    }

    /// Mutably borrow the value at the given index.
    pub fn at_mut(&mut self, index: i64) -> StrResult<&mut Value> {
        let len = self.len();
        self.locate(index)
            .and_then(move |i| self.0.make_mut().get_mut(i))
            .ok_or_else(|| out_of_bounds(index, len))
    }

    /// Push a value to the end of the array.
    pub fn push(&mut self, value: Value) {
        self.0.push(value);
    }

    /// Remove the last value in the array.
    pub fn pop(&mut self) -> StrResult<Value> {
        self.0.pop().ok_or_else(array_is_empty)
    }

    /// Insert a value at the specified index.
    pub fn insert(&mut self, index: i64, value: Value) -> StrResult<()> {
        let len = self.len();
        let i = self
            .locate(index)
            .filter(|&i| i <= self.0.len())
            .ok_or_else(|| out_of_bounds(index, len))?;

        self.0.insert(i, value);
        Ok(())
    }

    /// Remove and return the value at the specified index.
    pub fn remove(&mut self, index: i64) -> StrResult<Value> {
        let len = self.len();
        let i = self
            .locate(index)
            .filter(|&i| i < self.0.len())
            .ok_or_else(|| out_of_bounds(index, len))?;

        Ok(self.0.remove(i))
    }

    /// Extract a contigous subregion of the array.
    pub fn slice(&self, start: i64, end: Option<i64>) -> StrResult<Self> {
        let len = self.len();
        let start = self
            .locate(start)
            .filter(|&start| start <= self.0.len())
            .ok_or_else(|| out_of_bounds(start, len))?;

        let end = end.unwrap_or(self.len());
        let end = self
            .locate(end)
            .filter(|&end| end <= self.0.len())
            .ok_or_else(|| out_of_bounds(end, len))?
            .max(start);

        Ok(Self::from_vec(self.0[start..end].into()))
    }

    /// Whether the array contains a specific value.
    pub fn contains(&self, value: &Value) -> bool {
        self.0.contains(value)
    }

    /// Return the first matching element.
    pub fn find(&self, vm: &mut Vm, func: Func) -> SourceResult<Option<Value>> {
        if func.argc().map_or(false, |count| count != 1) {
            bail!(func.span(), "function must have exactly one parameter");
        }
        for item in self.iter() {
            let args = Args::new(func.span(), [item.clone()]);
            if func.call(vm, args)?.cast::<bool>().at(func.span())? {
                return Ok(Some(item.clone()));
            }
        }

        Ok(None)
    }

    /// Return the index of the first matching element.
    pub fn position(&self, vm: &mut Vm, func: Func) -> SourceResult<Option<i64>> {
        if func.argc().map_or(false, |count| count != 1) {
            bail!(func.span(), "function must have exactly one parameter");
        }
        for (i, item) in self.iter().enumerate() {
            let args = Args::new(func.span(), [item.clone()]);
            if func.call(vm, args)?.cast::<bool>().at(func.span())? {
                return Ok(Some(i as i64));
            }
        }

        Ok(None)
    }

    /// Return a new array with only those elements for which the function
    /// returns true.
    pub fn filter(&self, vm: &mut Vm, func: Func) -> SourceResult<Self> {
        if func.argc().map_or(false, |count| count != 1) {
            bail!(func.span(), "function must have exactly one parameter");
        }
        let mut kept = EcoVec::new();
        for item in self.iter() {
            let args = Args::new(func.span(), [item.clone()]);
            if func.call(vm, args)?.cast::<bool>().at(func.span())? {
                kept.push(item.clone())
            }
        }
        Ok(Self::from_vec(kept))
    }

    /// Transform each item in the array with a function.
    pub fn map(&self, vm: &mut Vm, func: Func) -> SourceResult<Self> {
        if func.argc().map_or(false, |count| !(1..=2).contains(&count)) {
            bail!(func.span(), "function must have one or two parameters");
        }
        let enumerate = func.argc() == Some(2);
        self.iter()
            .enumerate()
            .map(|(i, item)| {
                let mut args = Args::new(func.span(), []);
                if enumerate {
                    args.push(func.span(), Value::Int(i as i64));
                }
                args.push(func.span(), item.clone());
                func.call(vm, args)
            })
            .collect()
    }

    /// Fold all of the array's elements into one with a function.
    pub fn fold(&self, vm: &mut Vm, init: Value, func: Func) -> SourceResult<Value> {
        if func.argc().map_or(false, |count| count != 2) {
            bail!(func.span(), "function must have exactly two parameters");
        }
        let mut acc = init;
        for item in self.iter() {
            let args = Args::new(func.span(), [acc, item.clone()]);
            acc = func.call(vm, args)?;
        }
        Ok(acc)
    }

    /// Whether any element matches.
    pub fn any(&self, vm: &mut Vm, func: Func) -> SourceResult<bool> {
        if func.argc().map_or(false, |count| count != 1) {
            bail!(func.span(), "function must have exactly one parameter");
        }
        for item in self.iter() {
            let args = Args::new(func.span(), [item.clone()]);
            if func.call(vm, args)?.cast::<bool>().at(func.span())? {
                return Ok(true);
            }
        }

        Ok(false)
    }

    /// Whether all elements match.
    pub fn all(&self, vm: &mut Vm, func: Func) -> SourceResult<bool> {
        if func.argc().map_or(false, |count| count != 1) {
            bail!(func.span(), "function must have exactly one parameter");
        }
        for item in self.iter() {
            let args = Args::new(func.span(), [item.clone()]);
            if !func.call(vm, args)?.cast::<bool>().at(func.span())? {
                return Ok(false);
            }
        }

        Ok(true)
    }

    /// Return a new array with all items from this and nested arrays.
    pub fn flatten(&self) -> Self {
        let mut flat = EcoVec::with_capacity(self.0.len());
        for item in self.iter() {
            if let Value::Array(nested) = item {
                flat.extend(nested.flatten().into_iter());
            } else {
                flat.push(item.clone());
            }
        }
        Self::from_vec(flat)
    }

    /// Returns a new array with reversed order.
    pub fn rev(&self) -> Self {
        self.0.iter().cloned().rev().collect()
    }

    /// Join all values in the array, optionally with separator and last
    /// separator (between the final two items).
    pub fn join(&self, sep: Option<Value>, mut last: Option<Value>) -> StrResult<Value> {
        let len = self.0.len();
        let sep = sep.unwrap_or(Value::None);

        let mut result = Value::None;
        for (i, value) in self.iter().cloned().enumerate() {
            if i > 0 {
                if i + 1 == len && last.is_some() {
                    result = ops::join(result, last.take().unwrap())?;
                } else {
                    result = ops::join(result, sep.clone())?;
                }
            }

            result = ops::join(result, value)?;
        }

        Ok(result)
    }

    /// Return a sorted version of this array.
    ///
    /// Returns an error if two values could not be compared.
    pub fn sorted(&self) -> StrResult<Self> {
        let mut result = Ok(());
        let mut vec = self.0.clone();
        vec.make_mut().sort_by(|a, b| {
            a.partial_cmp(b).unwrap_or_else(|| {
                if result.is_ok() {
                    result = Err(eco_format!(
                        "cannot order {} and {}",
                        a.type_name(),
                        b.type_name(),
                    ));
                }
                Ordering::Equal
            })
        });
        result.map(|_| Self::from_vec(vec))
    }

    /// Repeat this array `n` times.
    pub fn repeat(&self, n: i64) -> StrResult<Self> {
        let count = usize::try_from(n)
            .ok()
            .and_then(|n| self.0.len().checked_mul(n))
            .ok_or_else(|| format!("cannot repeat this array {} times", n))?;

        Ok(self.iter().cloned().cycle().take(count).collect())
    }

    /// Extract a slice of the whole array.
    pub fn as_slice(&self) -> &[Value] {
        self.0.as_slice()
    }

    /// Iterate over references to the contained values.
    pub fn iter(&self) -> std::slice::Iter<Value> {
        self.0.iter()
    }

    /// Resolve an index.
    fn locate(&self, index: i64) -> Option<usize> {
        usize::try_from(if index >= 0 { index } else { self.len().checked_add(index)? })
            .ok()
    }
}

/// The out of bounds access error message.
#[cold]
fn out_of_bounds(index: i64, len: i64) -> EcoString {
    eco_format!("array index out of bounds (index: {}, len: {})", index, len)
}

/// The error message when the array is empty.
#[cold]
fn array_is_empty() -> EcoString {
    "array is empty".into()
}

impl Debug for Array {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        f.write_char('(')?;
        for (i, value) in self.iter().enumerate() {
            value.fmt(f)?;
            if i + 1 < self.0.len() {
                f.write_str(", ")?;
            }
        }
        if self.len() == 1 {
            f.write_char(',')?;
        }
        f.write_char(')')
    }
}

impl Add for Array {
    type Output = Self;

    fn add(mut self, rhs: Array) -> Self::Output {
        self += rhs;
        self
    }
}

impl AddAssign for Array {
    fn add_assign(&mut self, rhs: Array) {
        self.0.extend(rhs.0);
    }
}

impl Extend<Value> for Array {
    fn extend<T: IntoIterator<Item = Value>>(&mut self, iter: T) {
        self.0.extend(iter);
    }
}

impl FromIterator<Value> for Array {
    fn from_iter<T: IntoIterator<Item = Value>>(iter: T) -> Self {
        Self(iter.into_iter().collect())
    }
}

impl IntoIterator for Array {
    type Item = Value;
    type IntoIter = ecow::vec::IntoIter<Value>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

impl<'a> IntoIterator for &'a Array {
    type Item = &'a Value;
    type IntoIter = std::slice::Iter<'a, Value>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}
