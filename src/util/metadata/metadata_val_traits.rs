use core::sync::atomic::*;
use num_traits::Unsigned;
use num_traits::{FromPrimitive, ToPrimitive};
use crate::util::Address;

/// Describes bits and log2 bits for the numbers.
/// If num_traits has this, we do not need our own implementation: https://github.com/rust-num/num-traits/issues/247
pub trait Bits {
    const BITS: u32;
    const LOG2: u32;
}
macro_rules! impl_bits_trait {
    ($t: ty) => {
        impl Bits for $t {
            const BITS: u32 = <$t>::BITS;
            const LOG2: u32 = Self::BITS.trailing_zeros();
        }
    }
}
impl_bits_trait!(u8);
impl_bits_trait!(u16);
impl_bits_trait!(u32);
impl_bits_trait!(u64);
impl_bits_trait!(usize);

pub trait BitwiseOps {
    fn bitand(self, other: Self) -> Self;
    fn bitor(self, other: Self) -> Self;
    fn bitxor(self, other: Self) -> Self;
    fn inv(self) -> Self;
}
macro_rules! impl_bitwise_ops_trait {
    ($t: ty) => {
        impl BitwiseOps for $t {
            fn bitand(self, other: Self) -> Self {
                self & other
            }
            fn bitor(self, other: Self) -> Self {
                self | other
            }
            fn bitxor(self, other: Self) -> Self {
                self ^ other
            }
            fn inv(self) -> Self {
                !self
            }
        }
    }
}
impl_bitwise_ops_trait!(u8);
impl_bitwise_ops_trait!(u16);
impl_bitwise_ops_trait!(u32);
impl_bitwise_ops_trait!(u64);
impl_bitwise_ops_trait!(usize);

/// Atomic trait used for metadata.
/// Ideally we should use atomic_traits or atomic. However, for those traits,
/// their associate non-atomic type is a general type rather than a type of numbers.
// pub trait MetadataAtomic: Sized {
//     type NonAtomicType: MetadataValue;

//     fn load(&self, order: Ordering) -> Self::NonAtomicType;
//     fn store(&self, value: Self::NonAtomicType, order: Ordering);
//     fn compare_exchange(
//         &self,
//         current: Self::NonAtomicType,
//         new: Self::NonAtomicType,
//         success: Ordering,
//         failure: Ordering,
//     ) -> Result<Self::NonAtomicType, Self::NonAtomicType>;
//     fn fetch_add(&self, value: Self::NonAtomicType, order: Ordering) -> Self::NonAtomicType;
//     fn fetch_sub(&self, value: Self::NonAtomicType, order: Ordering) -> Self::NonAtomicType;
//     fn fetch_update<F>(
//         &self,
//         set_order: Ordering,
//         fetch_order: Ordering,
//         f: F,
//     ) -> Result<Self::NonAtomicType, Self::NonAtomicType>
//     where
//         F: FnMut(Self::NonAtomicType) -> Option<Self::NonAtomicType>;
// }
// macro_rules! impl_atomic_trait {
//     ($atomic:ty, $non_atomic:ty) => {
//         impl MetadataAtomic for $atomic {
//             type NonAtomicType = $non_atomic;

//             #[inline]
//             fn load(&self, order: Ordering) -> Self::NonAtomicType {
//                 <$atomic>::load(self, order)
//             }

//             #[inline]
//             fn store(&self, value: Self::NonAtomicType, order: Ordering) {
//                 <$atomic>::store(self, value, order)
//             }

//             #[inline]
//             fn compare_exchange(
//                 &self,
//                 current: Self::NonAtomicType,
//                 new: Self::NonAtomicType,
//                 success: Ordering,
//                 failure: Ordering,
//             ) -> Result<Self::NonAtomicType, Self::NonAtomicType> {
//                 <$atomic>::compare_exchange(
//                     self,
//                     current,
//                     new,
//                     success,
//                     failure,
//                 )
//             }

//             #[inline]
//             fn fetch_add(&self, value: Self::NonAtomicType, order: Ordering) -> Self::NonAtomicType{
//                 <$atomic>::fetch_add(self, value, order)
//             }

//             #[inline]
//             fn fetch_sub(&self, value: Self::NonAtomicType, order: Ordering) -> Self::NonAtomicType{
//                 <$atomic>::fetch_sub(self, value, order)
//             }

//             #[inline]
//             fn fetch_update<F>(
//                 &self,
//                 set_order: Ordering,
//                 fetch_order: Ordering,
//                 f: F,
//             ) -> Result<Self::NonAtomicType, Self::NonAtomicType>
//             where
//                 F: FnMut(Self::NonAtomicType) -> Option<Self::NonAtomicType> {
//                 <$atomic>::fetch_update(self, set_order, fetch_order, f)
//             }
//         }
//     }
// }
// impl_atomic_trait!(AtomicU8, u8);
// impl_atomic_trait!(AtomicU16, u16);
// impl_atomic_trait!(AtomicU32, u32);
// impl_atomic_trait!(AtomicU64, u64);

/// The number type for load/store metadata.
pub trait MetadataValue: Unsigned + Bits + BitwiseOps + ToPrimitive + Copy + FromPrimitive + std::fmt::Display + std::fmt::Debug {
    // type AtomicType: MetadataAtomic;
    // fn as_atomic(&self) -> &Self::AtomicType;
    fn load(addr: Address) -> Self;
    fn load_atomic(addr: Address, order: Ordering) -> Self;
    fn store(addr: Address, value: Self);
    fn store_atomic(addr: Address, value: Self, order: Ordering);
    fn compare_exchange(addr: Address, current: Self, new: Self, success: Ordering, failure: Ordering) -> Result<Self, Self>;
    fn fetch_add(addr: Address, value: Self, order: Ordering) -> Self;
    fn fetch_sub(addr: Address, value: Self, order: Ordering) -> Self;
    fn fetch_update<F>(addr: Address, set_order: Ordering, fetch_order: Ordering, f: F) -> Result<Self, Self> where F: FnMut(Self) -> Option<Self>;
}
macro_rules! impl_metadata_value_trait {
    ($non_atomic: ty, $atomic: ty) => {
        impl MetadataValue for $non_atomic {
            // type AtomicType = $atomic;

            // #[inline(always)]
            // fn as_atomic(&self) -> &$atomic {
            //     unsafe { std::mem::transmute(self) }
            // }

            #[inline]
            fn load(addr: Address) -> Self {
                unsafe { addr.load::<$non_atomic>() }
            }

            #[inline]
            fn load_atomic(addr: Address, order: Ordering) -> Self {
                unsafe { addr.as_ref::<$atomic>() }.load(order)
            }

            #[inline]
            fn store(addr: Address, value: Self) {
                unsafe { addr.store::<$non_atomic>(value) }
            }

            #[inline]
            fn store_atomic(addr: Address, value: Self, order: Ordering) {
                unsafe { addr.as_ref::<$atomic>() }.store(value, order)
            }

            #[inline]
            fn compare_exchange(
                addr: Address,
                current: Self,
                new: Self,
                success: Ordering,
                failure: Ordering,
            ) -> Result<Self, Self> {
                unsafe { addr.as_ref::<$atomic>() }.compare_exchange(
                    current,
                    new,
                    success,
                    failure,
                )
            }

            #[inline]
            fn fetch_add(addr: Address, value: Self, order: Ordering) -> Self{
                unsafe { addr.as_ref::<$atomic>() }.fetch_add(value, order)
            }

            #[inline]
            fn fetch_sub(addr: Address, value: Self, order: Ordering) -> Self{
                unsafe { addr.as_ref::<$atomic>() }.fetch_sub(value, order)
            }

            #[inline]
            fn fetch_update<F>(
                addr: Address,
                set_order: Ordering,
                fetch_order: Ordering,
                f: F,
            ) -> Result<Self, Self>
            where
                F: FnMut(Self) -> Option<Self> {
                unsafe { addr.as_ref::<$atomic>() }.fetch_update(set_order, fetch_order, f)
            }
        }
    }
}
impl_metadata_value_trait!(u8, AtomicU8);
impl_metadata_value_trait!(u16, AtomicU16);
impl_metadata_value_trait!(u32, AtomicU32);
impl_metadata_value_trait!(u64, AtomicU64);
impl_metadata_value_trait!(usize, AtomicUsize);
