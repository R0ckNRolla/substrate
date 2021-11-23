// This file is part of Substrate.

// Copyright (C) 2019-2021 Parity Technologies (UK) Ltd.
// SPDX-License-Identifier: Apache-2.0

// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
// 	http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! Traits and associated utilities for scheduling dispatchables in FRAME.

use codec::{Codec, Decode, Encode, EncodeLike, MaxEncodedLen};
use scale_info::TypeInfo;
use sp_runtime::{DispatchError, RuntimeDebug};
use sp_std::{fmt::Debug, result::Result, prelude::*};

/// Information relating to the period of a scheduled task. First item is the length of the
/// period and the second is the number of times it should be executed in total before the task
/// is considered finished and removed.
pub type Period<BlockNumber> = (BlockNumber, u32);

/// Priority with which a call is scheduled. It's just a linear amount with lowest values meaning
/// higher priority.
pub type Priority = u8;

/// The dispatch time of a scheduled task.
#[derive(Encode, Decode, Copy, Clone, PartialEq, Eq, RuntimeDebug, TypeInfo)]
pub enum DispatchTime<BlockNumber> {
	/// At specified block.
	At(BlockNumber),
	/// After specified number of blocks.
	After(BlockNumber),
}

/// The highest priority. We invert the value so that normal sorting will place the highest
/// priority at the beginning of the list.
pub const HIGHEST_PRIORITY: Priority = 0;
/// Anything of this value or lower will definitely be scheduled on the block that they ask for,
/// even if it breaches the `MaximumWeight` limitation.
pub const HARD_DEADLINE: Priority = 63;
/// The lowest priority. Most stuff should be around here.
pub const LOWEST_PRIORITY: Priority = 255;

/// Type representing a call. Can be either the `Call` value itself or the hash of the encoded
/// `Call`.
#[derive(Clone, Eq, PartialEq, Encode, Decode, RuntimeDebug, TypeInfo, MaxEncodedLen)]
pub enum CallOrHash<Call, Hash> {
	/// The `Call` value itself.
	Call(Call),
	/// The hash of the encoded `Call` which this value represents.
	Hash(Hash),
}

impl<C, H> From<C> for CallOrHash<C, H> {
	fn from(c: C) -> Self {
		CallOrHash::Call(c)
	}
}

/// Error type for `CallOrHash::lookup`.
#[derive(Clone, Eq, PartialEq, Encode, Decode, RuntimeDebug, TypeInfo, MaxEncodedLen)]
pub enum LookupError {
	/// A call of this hash was not known.
	Unknown,
	/// The preimage for this hash was known but could not be decoded into a `Call`.
	BadFormat,
}

impl<C: Decode, H> CallOrHash<C, H> {
	pub fn as_call(&self) -> Option<&C> {
		match &self {
			Self::Call(c) => Some(c),
			Self::Hash(_) => None,
		}
	}

	pub fn resolved<P: PreimageProvider<H>>(self) -> Self {
		match self {
			Self::Call(c) => Self::Call(c),
			Self::Hash(h) => {
				let data = match P::get_preimage(&h) {
					Some(p) => p,
					None => return Self::Hash(h),
				};
				match C::decode(&mut &data[..]) {
					Ok(c) => Self::Call(c),
					Err(_) => Self::Hash(h),
				}
			}
		}
	}

	pub fn try_resolve<P: PreimageProvider<H>>(self) -> Result<C, LookupError> {
		match self {
			Self::Call(c) => Ok(c),
			Self::Hash(h) => {
				let data = P::get_preimage(&h).ok_or(LookupError::Unknown)?;
				C::decode(&mut &data[..]).map_err(|_| LookupError::BadFormat)
			}
		}
	}
}

pub mod v1 {
	use super::*;

	/// A type that can be used as a scheduler.
	pub trait Anon<BlockNumber, Call, Origin> {
		/// An address which can be used for removing a scheduled task.
		type Address: Codec + Clone + Eq + EncodeLike + Debug;

		/// Schedule a dispatch to happen at the beginning of some block in the future.
		///
		/// This is not named.
		fn schedule(
			when: DispatchTime<BlockNumber>,
			maybe_periodic: Option<Period<BlockNumber>>,
			priority: Priority,
			origin: Origin,
			call: Call,
		) -> Result<Self::Address, DispatchError>;

		/// Cancel a scheduled task. If periodic, then it will cancel all further instances of that,
		/// also.
		///
		/// Will return an error if the `address` is invalid.
		///
		/// NOTE: This guaranteed to work only *before* the point that it is due to be executed.
		/// If it ends up being delayed beyond the point of execution, then it cannot be cancelled.
		///
		/// NOTE2: This will not work to cancel periodic tasks after their initial execution. For
		/// that, you must name the task explicitly using the `Named` trait.
		fn cancel(address: Self::Address) -> Result<(), ()>;

		/// Reschedule a task. For one-off tasks, this dispatch is guaranteed to succeed
		/// only if it is executed *before* the currently scheduled block. For periodic tasks,
		/// this dispatch is guaranteed to succeed only before the *initial* execution; for
		/// others, use `reschedule_named`.
		///
		/// Will return an error if the `address` is invalid.
		fn reschedule(
			address: Self::Address,
			when: DispatchTime<BlockNumber>,
		) -> Result<Self::Address, DispatchError>;

		/// Return the next dispatch time for a given task.
		///
		/// Will return an error if the `address` is invalid.
		fn next_dispatch_time(address: Self::Address) -> Result<BlockNumber, ()>;
	}

	/// A type that can be used as a scheduler.
	pub trait Named<BlockNumber, Call, Origin> {
		/// An address which can be used for removing a scheduled task.
		type Address: Codec + Clone + Eq + EncodeLike + sp_std::fmt::Debug;

		/// Schedule a dispatch to happen at the beginning of some block in the future.
		///
		/// - `id`: The identity of the task. This must be unique and will return an error if not.
		fn schedule_named(
			id: Vec<u8>,
			when: DispatchTime<BlockNumber>,
			maybe_periodic: Option<Period<BlockNumber>>,
			priority: Priority,
			origin: Origin,
			call: Call,
		) -> Result<Self::Address, ()>;

		/// Cancel a scheduled, named task. If periodic, then it will cancel all further instances
		/// of that, also.
		///
		/// Will return an error if the `id` is invalid.
		///
		/// NOTE: This guaranteed to work only *before* the point that it is due to be executed.
		/// If it ends up being delayed beyond the point of execution, then it cannot be cancelled.
		fn cancel_named(id: Vec<u8>) -> Result<(), ()>;

		/// Reschedule a task. For one-off tasks, this dispatch is guaranteed to succeed
		/// only if it is executed *before* the currently scheduled block.
		fn reschedule_named(
			id: Vec<u8>,
			when: DispatchTime<BlockNumber>,
		) -> Result<Self::Address, DispatchError>;

		/// Return the next dispatch time for a given task.
		///
		/// Will return an error if the `id` is invalid.
		fn next_dispatch_time(id: Vec<u8>) -> Result<BlockNumber, ()>;
	}

	impl<T, BlockNumber, Call, Origin> Anon<BlockNumber, Call, Origin> for T where
		T: v2::Anon<BlockNumber, Call, Origin>,
	{
		type Address = T::Address;

		fn schedule(
			when: DispatchTime<BlockNumber>,
			maybe_periodic: Option<Period<BlockNumber>>,
			priority: Priority,
			origin: Origin,
			call: Call,
		) -> Result<Self::Address, DispatchError> {
			let c = CallOrHash::<Call, T::Hash>::Call(call);
			T::schedule(when, maybe_periodic, priority, origin, c)
		}

		fn cancel(address: Self::Address) -> Result<(), ()> {
			T::cancel(address)
		}

		fn reschedule(
			address: Self::Address,
			when: DispatchTime<BlockNumber>,
		) -> Result<Self::Address, DispatchError> {
			T::reschedule(address, when)
		}

		fn next_dispatch_time(address: Self::Address) -> Result<BlockNumber, ()> {
			T::next_dispatch_time(address)
		}
	}

	impl<T, BlockNumber, Call, Origin> Named<BlockNumber, Call, Origin> for T where
		T: v2::Named<BlockNumber, Call, Origin>,
	{
		type Address = T::Address;

		fn schedule_named(
			id: Vec<u8>,
			when: DispatchTime<BlockNumber>,
			maybe_periodic: Option<Period<BlockNumber>>,
			priority: Priority,
			origin: Origin,
			call: Call,
		) -> Result<Self::Address, ()> {
			let c = CallOrHash::<Call, T::Hash>::Call(call);
			T::schedule_named(id, when, maybe_periodic, priority, origin, c)
		}

		fn cancel_named(id: Vec<u8>) -> Result<(), ()> {
			T::cancel_named(id)
		}

		fn reschedule_named(
			id: Vec<u8>,
			when: DispatchTime<BlockNumber>,
		) -> Result<Self::Address, DispatchError> {
			T::reschedule_named(id, when)
		}

		fn next_dispatch_time(id: Vec<u8>) -> Result<BlockNumber, ()> {
			T::next_dispatch_time(id)
		}
	}
}

pub mod v2 {
	use super::*;

	/// A type that can be used as a scheduler.
	pub trait Anon<BlockNumber, Call, Origin> {
		/// An address which can be used for removing a scheduled task.
		type Address: Codec + Clone + Eq + EncodeLike + Debug;
		/// A means of expressing a call by the hash of its encoded data.
		type Hash;

		/// Schedule a dispatch to happen at the beginning of some block in the future.
		///
		/// This is not named.
		fn schedule(
			when: DispatchTime<BlockNumber>,
			maybe_periodic: Option<Period<BlockNumber>>,
			priority: Priority,
			origin: Origin,
			call: CallOrHash<Call, Self::Hash>,
		) -> Result<Self::Address, DispatchError>;

		/// Cancel a scheduled task. If periodic, then it will cancel all further instances of that,
		/// also.
		///
		/// Will return an error if the `address` is invalid.
		///
		/// NOTE: This guaranteed to work only *before* the point that it is due to be executed.
		/// If it ends up being delayed beyond the point of execution, then it cannot be cancelled.
		///
		/// NOTE2: This will not work to cancel periodic tasks after their initial execution. For
		/// that, you must name the task explicitly using the `Named` trait.
		fn cancel(address: Self::Address) -> Result<(), ()>;

		/// Reschedule a task. For one-off tasks, this dispatch is guaranteed to succeed
		/// only if it is executed *before* the currently scheduled block. For periodic tasks,
		/// this dispatch is guaranteed to succeed only before the *initial* execution; for
		/// others, use `reschedule_named`.
		///
		/// Will return an error if the `address` is invalid.
		fn reschedule(
			address: Self::Address,
			when: DispatchTime<BlockNumber>,
		) -> Result<Self::Address, DispatchError>;

		/// Return the next dispatch time for a given task.
		///
		/// Will return an error if the `address` is invalid.
		fn next_dispatch_time(address: Self::Address) -> Result<BlockNumber, ()>;
	}

	/// A type that can be used as a scheduler.
	pub trait Named<BlockNumber, Call, Origin> {
		/// An address which can be used for removing a scheduled task.
		type Address: Codec + Clone + Eq + EncodeLike + sp_std::fmt::Debug;
		/// A means of expressing a call by the hash of its encoded data.
		type Hash;

		/// Schedule a dispatch to happen at the beginning of some block in the future.
		///
		/// - `id`: The identity of the task. This must be unique and will return an error if not.
		fn schedule_named(
			id: Vec<u8>,
			when: DispatchTime<BlockNumber>,
			maybe_periodic: Option<Period<BlockNumber>>,
			priority: Priority,
			origin: Origin,
			call: CallOrHash<Call, Self::Hash>,
		) -> Result<Self::Address, ()>;

		/// Cancel a scheduled, named task. If periodic, then it will cancel all further instances
		/// of that, also.
		///
		/// Will return an error if the `id` is invalid.
		///
		/// NOTE: This guaranteed to work only *before* the point that it is due to be executed.
		/// If it ends up being delayed beyond the point of execution, then it cannot be cancelled.
		fn cancel_named(id: Vec<u8>) -> Result<(), ()>;

		/// Reschedule a task. For one-off tasks, this dispatch is guaranteed to succeed
		/// only if it is executed *before* the currently scheduled block.
		fn reschedule_named(
			id: Vec<u8>,
			when: DispatchTime<BlockNumber>,
		) -> Result<Self::Address, DispatchError>;

		/// Return the next dispatch time for a given task.
		///
		/// Will return an error if the `id` is invalid.
		fn next_dispatch_time(id: Vec<u8>) -> Result<BlockNumber, ()>;
	}
}

pub use v1::*;

use super::PreimageProvider;
