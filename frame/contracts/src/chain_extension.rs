// This file is part of Substrate.

// Copyright (C) 2020 Parity Technologies (UK) Ltd.
// SPDX-License-Identifier: Apache-2.0

// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
// http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! A mechanism for runtime authors to augment the functionality of contracts.
//!
//! The runtime is able to call into any contract and retrieve the result using
//! [`bare_call`](crate::Pallet::bare_call). This already allows customization of runtime
//! behaviour by user generated code (contracts). However, often it is more straightforward
//! to allow the reverse behaviour: The contract calls into the runtime. We call the latter
//! one a "chain extension" because it allows the chain to extend the set of functions that are
//! callable by a contract.
//!
//! In order to create a chain extension the runtime author implements the [`ChainExtension`]
//! trait and declares it in this pallet's [configuration Trait](crate::Config). All types
//! required for this endeavour are defined or re-exported in this module. There is an
//! implementation on `()` which can be used to signal that no chain extension is available.
//!
//! # Security
//!
//! The chain author alone is responsible for the security of the chain extension.
//! This includes avoiding the exposure of exploitable functions and charging the
//! appropriate amount of weight. In order to do so benchmarks must be written and the
//! [`charge_weight`](Environment::charge_weight) function must be called **before**
//! carrying out any action that causes the consumption of the chargeable weight.
//! It cannot be overstated how delicate of a process the creation of a chain extension
//! is. Check whether using [`bare_call`](crate::Pallet::bare_call) suffices for the
//! use case at hand.
//!
//! # Benchmarking
//!
//! The builtin contract callable functions that pallet-contracts provides all have
//! benchmarks that determine the correct weight that an invocation of these functions
//! induces. In order to be able to charge the correct weight for the functions defined
//! by a chain extension benchmarks must be written, too. In the near future this crate
//! will provide the means for easier creation of those specialized benchmarks.
//!
//! # Example
//!
//! The ink! repository maintains an
//! [end-to-end example](https://github.com/paritytech/ink/tree/master/examples/rand-extension)
//! on how to use a chain extension in order to provide new features to ink! contracts.

use crate::{
	Error,
	wasm::{Runtime, RuntimeToken},
};
use codec::Decode;
use frame_support::weights::Weight;
use sp_runtime::DispatchError;
use sp_std::{
	marker::PhantomData,
	vec::Vec,
};

pub use frame_system::Config as SysConfig;
pub use pallet_contracts_primitives::ReturnFlags;
pub use sp_core::crypto::UncheckedFrom;
pub use crate::{Config, exec::Ext};
pub use state::Init as InitState;

/// Result that returns a [`DispatchError`] on error.
pub type Result<T> = sp_std::result::Result<T, DispatchError>;

/// A trait used to extend the set of contract callable functions.
///
/// In order to create a custom chain extension this trait must be implemented and supplied
/// to the pallet contracts configuration trait as the associated type of the same name.
/// Consult the [module documentation](self) for a general explanation of chain extensions.
pub trait ChainExtension<C: Config> {
	/// Call the chain extension logic.
	///
	/// This is the only function that needs to be implemented in order to write a
	/// chain extensions. It is called whenever a contract calls the `seal_call_chain_extension`
	/// imported wasm function.
	///
	/// # Parameters
	/// - `func_id`: The first argument to `seal_call_chain_extension`. Usually used to
	///		determine which function to realize.
	/// - `env`: Access to the remaining arguments and the execution environment.
	///
	/// # Return
	///
	/// In case of `Err` the contract execution is immediately suspended and the passed error
	/// is returned to the caller. Otherwise the value of [`RetVal`] determines the exit
	/// behaviour.
	fn call<E>(func_id: u32, env: Environment<E, InitState>) -> Result<RetVal>
	where
		E: Ext<T = C>,
		<E::T as SysConfig>::AccountId: UncheckedFrom<<E::T as SysConfig>::Hash> + AsRef<[u8]>;

	/// Determines whether chain extensions are enabled for this chain.
	///
	/// The default implementation returns `true`. Therefore it is not necessary to overwrite
	/// this function when implementing a chain extension. In case of `false` the deployment of
	/// a contract that references `seal_call_chain_extension` will be denied and calling this
	/// function will return [`NoChainExtension`](Error::NoChainExtension) without first calling
	/// into [`call`](Self::call).
	fn enabled() -> bool {
		true
	}
}

/// Implementation that indicates that no chain extension is available.
impl<C: Config> ChainExtension<C> for () {
	fn call<E>(_func_id: u32, mut _env: Environment<E, InitState>) -> Result<RetVal>
	where
		E: Ext<T = C>,
		<E::T as SysConfig>::AccountId: UncheckedFrom<<E::T as SysConfig>::Hash> + AsRef<[u8]>,
	{
		// Never called since [`Self::enabled()`] is set to `false`. Because we want to
		// avoid panics at all costs we supply a sensible error value here instead
		// of an `unimplemented!`.
		Err(Error::<E::T>::NoChainExtension.into())
	}

	fn enabled() -> bool {
		false
	}
}

/// Determines the exit behaviour and return value of a chain extension.
pub enum RetVal {
	/// The chain extensions returns the supplied value to its calling contract.
	Converging(u32),
	/// The control does **not** return to the calling contract.
	///
	/// Use this to stop the execution of the contract when the chain extension returns.
	/// The semantic is the same as for calling `seal_return`: The control returns to
	/// the caller of the currently executing contract yielding the supplied buffer and
	/// flags.
	Diverging{flags: ReturnFlags, data: Vec<u8>},
}

/// Grants the chain extension access to its parameters and execution environment.
///
/// It uses [typestate programming](https://docs.rust-embedded.org/book/static-guarantees/typestate-programming.html)
/// to enforce the correct usage of the parameters passed to the chain extension.
pub struct Environment<'a, 'b, E: Ext, S: state::State> {
	/// The actual data of this type.
	inner: Inner<'a, 'b, E>,
	/// `S` is only used in the type system but never as value.
	phantom: PhantomData<S>,
}

/// Functions that are available in every state of this type.
impl<'a, 'b, E: Ext, S: state::State> Environment<'a, 'b, E, S>
where
	<E::T as SysConfig>::AccountId: UncheckedFrom<<E::T as SysConfig>::Hash> + AsRef<[u8]>,
{
	/// Charge the passed `amount` of weight from the overall limit.
	///
	/// It returns `Ok` when there the remaining weight budget is larger than the passed
	/// `weight`. It returns `Err` otherwise. In this case the chain extension should
	/// abort the execution and pass through the error.
	///
	/// # Note
	///
	/// Weight is synonymous with gas in substrate.
	pub fn charge_weight(&mut self, amount: Weight) -> Result<()> {
		self.inner.runtime.charge_gas(RuntimeToken::ChainExtension(amount)).map(|_| ())
	}

	/// Grants access to the execution environment of the current contract call.
	///
	/// Consult the functions on the returned type before re-implementing those functions.
	pub fn ext(&mut self) -> &mut E {
		self.inner.runtime.ext()
	}
}

/// Functions that are only available in the initial state of this type.
///
/// Those are the functions that determine how the arguments to the chain extensions
/// should be consumed.
impl<'a, 'b, E: Ext> Environment<'a, 'b, E, state::Init> {
	/// Creates a new environment for consumption by a chain extension.
	///
	/// It is only available to this crate because only the wasm runtime module needs to
	/// ever create this type. Chain extensions merely consume it.
	pub(crate) fn new(
		runtime: &'a mut Runtime::<'b, E>,
		input_ptr: u32,
		input_len: u32,
		output_ptr: u32,
		output_len_ptr: u32,
	) -> Self {
		Environment {
			inner: Inner {
				runtime,
				input_ptr,
				input_len,
				output_ptr,
				output_len_ptr,
			},
			phantom: PhantomData,
		}
	}

	/// Use all arguments as integer values.
	pub fn only_in(self) -> Environment<'a, 'b, E, state::OnlyIn> {
		Environment {
			inner: self.inner,
			phantom: PhantomData,
		}
	}

	/// Use input arguments as integer and output arguments as pointer to a buffer.
	pub fn prim_in_buf_out(self) -> Environment<'a, 'b, E, state::PrimInBufOut> {
		Environment {
			inner: self.inner,
			phantom: PhantomData,
		}
	}

	/// Use input and output arguments as pointers to a buffer.
	pub fn buf_in_buf_out(self) -> Environment<'a, 'b, E, state::BufInBufOut> {
		Environment {
			inner: self.inner,
			phantom: PhantomData,
		}
	}
}

/// Functions to use the input arguments as integers.
impl<'a, 'b, E: Ext, S: state::PrimIn> Environment<'a, 'b, E, S> {
	/// The `input_ptr` argument.
	pub fn val0(&self) -> u32 {
		self.inner.input_ptr
	}

	/// The `input_len` argument.
	pub fn val1(&self) -> u32 {
		self.inner.input_len
	}
}

/// Functions to use the output arguments as integers.
impl<'a, 'b, E: Ext, S: state::PrimOut> Environment<'a, 'b, E, S> {
	/// The `output_ptr` argument.
	pub fn val2(&self) -> u32 {
		self.inner.output_ptr
	}

	/// The `output_len_ptr` argument.
	pub fn val3(&self) -> u32 {
		self.inner.output_len_ptr
	}
}

/// Functions to use the input arguments as pointer to a buffer.
impl<'a, 'b, E: Ext, S: state::BufIn> Environment<'a, 'b, E, S>
where
	<E::T as SysConfig>::AccountId: UncheckedFrom<<E::T as SysConfig>::Hash> + AsRef<[u8]>,
{
	/// Reads `min(max_len, in_len)` from contract memory.
	///
	/// This does **not** charge any weight. The caller must make sure that the an
	/// appropriate amount of weight is charged **before** reading from contract memory.
	/// The reason for that is that usually the costs for reading data and processing
	/// said data cannot be separated in a benchmark. Therefore a chain extension would
	/// charge the overall costs either using `max_len` (worst case approximation) or using
	/// [`in_len()`](Self::in_len).
	pub fn read(&self, max_len: u32) -> Result<Vec<u8>> {
		self.inner.runtime.read_sandbox_memory(
			self.inner.input_ptr,
			self.inner.input_len.min(max_len),
		)
	}

	/// Reads `min(buffer.len(), in_len) from contract memory.
	///
	/// This takes a mutable pointer to a buffer fills it with data and shrinks it to
	/// the size of the actual data. Apart from supporting pre-allocated buffers it is
	/// equivalent to to [`read()`](Self::read).
	pub fn read_into(&self, buffer: &mut &mut [u8]) -> Result<()> {
		let len = buffer.len();
		let sliced = {
			let buffer = core::mem::take(buffer);
			&mut buffer[..len.min(self.inner.input_len as usize)]
		};
		self.inner.runtime.read_sandbox_memory_into_buf(
			self.inner.input_ptr,
			sliced,
		)?;
		*buffer = sliced;
		Ok(())
	}

	/// Reads `in_len` from contract memory and scale decodes it.
	///
	/// This function is secure and recommended for all input types of fixed size
	/// as long as the cost of reading the memory is included in the overall already charged
	/// weight of the chain extension. This should usually be the case when fixed input types
	/// are used. Non fixed size types (like everything using `Vec`) usually need to use
	/// [`in_len()`](Self::in_len) in order to properly charge the necessary weight.
	pub fn read_as<T: Decode>(&mut self) -> Result<T> {
		self.inner.runtime.read_sandbox_memory_as(
			self.inner.input_ptr,
			self.inner.input_len,
		)
	}

	/// The length of the input as passed in as `input_len`.
	///
	/// A chain extension would use this value to calculate the dynamic part of its
	/// weight. For example a chain extension that calculates the hash of some passed in
	/// bytes would use `in_len` to charge the costs of hashing that amount of bytes.
	/// This also subsumes the act of copying those bytes as a benchmarks measures both.
	pub fn in_len(&self) -> u32 {
		self.inner.input_len
	}
}

/// Functions to use the output arguments as pointer to a buffer.
impl<'a, 'b, E: Ext, S: state::BufOut> Environment<'a, 'b, E, S>
where
	<E::T as SysConfig>::AccountId: UncheckedFrom<<E::T as SysConfig>::Hash> + AsRef<[u8]>,
{
	/// Write the supplied buffer to contract memory.
	///
	/// If the contract supplied buffer is smaller than the passed `buffer` an `Err` is returned.
	/// If `allow_skip` is set to true the contract is allowed to skip the copying of the buffer
	/// by supplying the guard value of `u32::max_value()` as `out_ptr`. The
	/// `weight_per_byte` is only charged when the write actually happens and is not skipped or
	/// failed due to a too small output buffer.
	pub fn write(
		&mut self,
		buffer: &[u8],
		allow_skip: bool,
		weight_per_byte: Option<Weight>,
	) -> Result<()> {
		self.inner.runtime.write_sandbox_output(
			self.inner.output_ptr,
			self.inner.output_len_ptr,
			buffer,
			allow_skip,
			|len| {
				weight_per_byte.map(|w| RuntimeToken::ChainExtension(w.saturating_mul(len.into())))
			},
		)
	}
}

/// The actual data of an `Environment`.
///
/// All data is put into this struct to easily pass it around as part of the typestate
/// pattern. Also it creates the opportunity to box this struct in the future in case it
/// gets too large.
struct Inner<'a, 'b, E: Ext> {
	/// The runtime contains all necessary functions to interact with the running contract.
	runtime: &'a mut Runtime::<'b, E>,
	/// Verbatim argument passed to `seal_call_chain_extension`.
	input_ptr: u32,
	/// Verbatim argument passed to `seal_call_chain_extension`.
	input_len: u32,
	/// Verbatim argument passed to `seal_call_chain_extension`.
	output_ptr: u32,
	/// Verbatim argument passed to `seal_call_chain_extension`.
	output_len_ptr: u32,
}

/// Private submodule with public types to prevent other modules from naming them.
mod state {
	pub trait State {}

	pub trait PrimIn: State {}
	pub trait PrimOut: State {}
	pub trait BufIn: State {}
	pub trait BufOut: State {}

	/// The initial state of an [`Environment`](`super::Environment`).
	/// See [typestate programming](https://docs.rust-embedded.org/book/static-guarantees/typestate-programming.html).
	pub enum Init {}
	pub enum OnlyIn {}
	pub enum PrimInBufOut {}
	pub enum BufInBufOut {}

	impl State for Init {}
	impl State for OnlyIn {}
	impl State for PrimInBufOut {}
	impl State for BufInBufOut {}

	impl PrimIn for OnlyIn {}
	impl PrimOut for OnlyIn {}
	impl PrimIn for PrimInBufOut {}
	impl BufOut for PrimInBufOut {}
	impl BufIn for BufInBufOut {}
	impl BufOut for BufInBufOut {}
}
