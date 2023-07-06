use std::prelude::v1::*;

use numpy::{PyArray1, PyReadonlyArray1};
use pyo3::{prelude::*, types::PyTuple};

use crate::{
    stream::{Decode, Encode},
    Pos, Seek, UnwrapInfallible,
};

use super::model::{internals::EncoderDecoderModel, Model};

pub fn init_module(_py: Python<'_>, module: &PyModule) -> PyResult<()> {
    module.add_class::<AnsCoder>()?;
    Ok(())
}

/// An entropy coder based on [Asymmetric Numeral Systems (ANS)] [1].
///
/// This is a wrapper around the Rust type [`constriction::stream::stack::DefaultAnsCoder`]
/// with python bindings.
///
/// Note that this entropy coder is a stack (a "last in first out" data
/// structure). You can push symbols on the stack using the method`encode_reverse`,
/// and then pop them off *in reverse order* using the method `decode`.
///
/// To copy out the compressed data that is currently on the stack, call
/// `get_compressed`. You would typically want write this to a binary file in some
/// well-documented byte order. After reading it back in at a later time, you can
/// decompress it by constructing an `constriction.AnsCoder` where you pass in the compressed
/// data as an argument to the constructor.
///
/// If you're only interested in the compressed file size, calling `num_bits` will
/// be cheaper as it won't actually copy out the compressed data.
///
/// ## Examples
///
/// ### Compression:
///
/// ```python
/// import sys
/// import constriction
/// import numpy as np
///
/// ans = constriction.stream.stack.AnsCoder()  # No args => empty ANS coder
///
/// symbols = np.array([2, -1, 0, 2, 3], dtype=np.int32)
/// min_supported_symbol, max_supported_symbol = -10, 10  # both inclusively
/// model = constriction.stream.model.QuantizedGaussian(
///     min_supported_symbol, max_supported_symbol)
/// means = np.array([2.3, -1.7, 0.1, 2.2, -5.1], dtype=np.float64)
/// stds = np.array([1.1, 5.3, 3.8, 1.4, 3.9], dtype=np.float64)
///
/// ans.encode_reverse(symbols, model, means, stds)
///
/// print(f"Compressed size: {ans.num_valid_bits()} bits")
///
/// compressed = ans.get_compressed()
/// if sys.byteorder == "big":
///     # Convert native byte order to a consistent one (here: little endian).
///     compressed.byteswap(inplace=True)
/// compressed.tofile("compressed.bin")
/// ```
///
/// ### Decompression:
///
/// ```python
/// import sys
/// import constriction
/// import numpy as np
///
/// compressed = np.fromfile("compressed.bin", dtype=np.uint32)
/// if sys.byteorder == "big":
///     # Convert little endian byte order to native byte order.
///     compressed.byteswap(inplace=True)
///
/// ans = constriction.stream.stack.AnsCoder( compressed )
/// min_supported_symbol, max_supported_symbol = -10, 10  # both inclusively
/// model = constriction.stream.model.QuantizedGaussian(
///     min_supported_symbol, max_supported_symbol)
/// means = np.array([2.3, -1.7, 0.1, 2.2, -5.1], dtype=np.float64)
/// stds = np.array([1.1, 5.3, 3.8, 1.4, 3.9], dtype=np.float64)
///
/// reconstructed = ans.decode(model, means, stds)
/// assert ans.is_empty()
/// print(reconstructed)  # Should print [2, -1, 0, 2, 3]
/// ```
///
/// ## Constructor
///
/// AnsCoder(compressed)
///
/// Arguments:
/// compressed (optional) -- initial compressed data, as a numpy array with
///     dtype `uint32`.
///
/// [Asymmetric Numeral Systems (ANS)]: https://en.wikipedia.org/wiki/Asymmetric_numeral_systems
/// [`constriction::stream::ans::DefaultAnsCoder`]: crate::stream::stack::DefaultAnsCoder
///
/// ## References
///
/// [1] Duda, Jarek, et al. "The use of asymmetric numeral systems as an accurate
/// replacement for Huffman coding." 2015 Picture Coding Symposium (PCS). IEEE, 2015.
#[pyclass]
#[derive(Debug, Clone)]
pub struct AnsCoder {
    inner: crate::stream::stack::DefaultAnsCoder,
}

#[pymethods]
impl AnsCoder {
    /// The constructor has the call signature `AnsCoder([compressed, [seal=False]])`.
    ///
    /// - If you want to encode a message, call the constructor with no arguments.
    /// - If you want to decode a message that was previously encoded with an `AnsCoder`, call the
    ///   constructor with a single argument `compressed`, which must be a rank-1 numpy array with
    ///   `dtype=np.uint32` (as returned by the method
    ///   [`get_compressed`](#constriction.stream.stack.AnsCoder.get_compressed) when invoked with
    ///   no arguments).
    /// - For bits-back related compression techniques, it can sometimes be useful to decode symbols
    ///   from some arbitrary bit string that was *not* generated by ANS. To do so, call the
    ///   constructor with the additional argument `seal=True` (if you don't set `seal` to `True`
    ///   then the `AnsCoder` will truncate any trailing zero words from `compressed`). Once you've
    ///   decoded and re-encoded some symbols, you can get back the original `compressed` data by
    ///   calling `.get_compressed(unseal=True)`.
    #[new]
    #[pyo3(text_signature = "(self, [compressed], seal=False)")]
    pub fn new(
        compressed: Option<PyReadonlyArray1<'_, u32>>,
        seal: Option<bool>,
    ) -> PyResult<Self> {
        if compressed.is_none() && seal.is_some() {
            return Err(pyo3::exceptions::PyValueError::new_err(
                "Need compressed data to seal.",
            ));
        }
        let inner = if let Some(compressed) = compressed {
            let compressed = compressed.to_vec()?;
            if seal == Some(true) {
                crate::stream::stack::AnsCoder::from_binary(compressed).unwrap_infallible()
            } else {
                crate::stream::stack::AnsCoder::from_compressed(compressed).map_err(|_| {
                    pyo3::exceptions::PyValueError::new_err(
                        "Invalid compressed data: ANS compressed data never ends in a zero word.",
                    )
                })?
            }
        } else {
            crate::stream::stack::AnsCoder::new()
        };

        Ok(Self { inner })
    }

    /// Records a checkpoint to which you can jump during decoding using
    /// [`seek`](#constriction.stream.stack.AnsCoder.seek).
    ///
    /// Returns a tuple `(position, state)` where `position` is an integer that specifies how many
    /// 32-bit words of compressed data have been produced so far, and `state` is an integer that
    /// defines the `RangeEncoder`'s internal state (so that it can be restored upon
    /// [`seek`ing](#constriction.stream.stack.AnsCoder.seek).
    ///
    /// **Note:** Don't call `pos` if you just want to find out how much compressed data has been
    /// produced so far. Call [`num_words`](#constriction.stream.stack.AnsCoder.num_words)
    /// instead.
    ///
    /// ## Example
    ///
    /// See [`seek`](#constriction.stream.stack.AnsCoder.seek).
    #[pyo3(text_signature = "(self)")]
    pub fn pos(&mut self) -> (usize, u64) {
        self.inner.pos()
    }

    /// Jumps to a checkpoint recorded with method
    /// [`pos`](#constriction.stream.stack.AnsCoder.pos) during encoding.
    ///
    /// This allows random-access decoding. The arguments `position` and `state` are the two values
    /// returned by the method [`pos`](#constriction.stream.stack
    ///
    /// **Note:** in an ANS coder, both decoding and seeking *consume* compressed data. The Python
    /// API of `constriction`'s ANS coder currently supports only seeking forward but not backward
    /// (seeking backward is supported for Range Coding, and for both ANS and Range Coding in
    /// `constriction`'s Rust API).
    ///
    /// ## Example
    ///
    /// ```python
    /// probabilities = np.array([0.2, 0.4, 0.1, 0.3], dtype=np.float64)
    /// model         = constriction.stream.model.Categorical(probabilities)
    /// message_part1 = np.array([1, 2, 0, 3, 2, 3, 0], dtype=np.int32)
    /// message_part2 = np.array([2, 2, 0, 1, 3], dtype=np.int32)
    ///
    /// # Encode both parts of the message (in reverse order, because ANS
    /// # operates as a stack) and record a checkpoint in-between:
    /// coder = constriction.stream.stack.AnsCoder()
    /// coder.encode_reverse(message_part2, model)
    /// (position, state) = coder.pos() # Records a checkpoint.
    /// coder.encode_reverse(message_part1, model)
    ///
    /// # We could now call `coder.get_compressed()` but we'll just decode
    /// # directly from the original `coder` for simplicity.
    ///
    /// # Decode first symbol:
    /// print(coder.decode(model)) # (prints: 1)
    ///
    /// # Jump to part 2 and decode it:
    /// coder.seek(position, state)
    /// decoded_part2 = coder.decode(model, 5)
    /// assert np.all(decoded_part2 == message_part2)
    /// ```
    #[pyo3(text_signature = "(self, position, state)")]
    pub fn seek(&mut self, position: usize, state: u64) -> PyResult<()> {
        self.inner.seek((position, state)).map_err(|()| {
            pyo3::exceptions::PyAttributeError::new_err(
                "Tried to seek past end of stream. Note: in an ANS coder,\n\
                both decoding and seeking *consume* compressed data. The Python API of\n\
                `constriction`'s ANS coder currently does not support seeking backward.",
            )
        })
    }

    /// Resets the encoder to an empty state.
    ///
    /// This removes any existing compressed data on the encoder. It is equivalent to replacing the
    /// encoder with a new one but slightly more efficient.
    #[pyo3(text_signature = "(self)")]
    pub fn clear(&mut self) {
        self.inner.clear();
    }

    /// Returns the current size of the encapsulated compressed data, in `np.uint32` words.
    ///
    /// Thus, the number returned by this method is the length of the array that you would get if
    /// you called [`get_compressed`](#constriction.stream.queue.RangeEncoder.get_compressed)
    /// without arguments.
    #[pyo3(text_signature = "(self)")]
    pub fn num_words(&self) -> usize {
        self.inner.num_words()
    }

    /// Returns the current size of the compressed data, in bits, rounded up to full words.
    ///
    /// This is 32 times the result of what [`num_words`](#constriction.stream.queue.RangeEncoder.num_words)
    /// would return.
    #[pyo3(text_signature = "(self)")]
    pub fn num_bits(&self) -> usize {
        self.inner.num_bits()
    }

    /// The current size of the compressed data, in bits, not rounded up to full words.
    ///
    /// This can be at most 32 smaller than `.num_bits()`.
    #[pyo3(text_signature = "(self)")]
    pub fn num_valid_bits(&self) -> usize {
        self.inner.num_valid_bits()
    }

    /// Returns `True` iff the coder is in its default initial state.
    ///
    /// The default initial state is the state returned by the constructor when
    /// called without arguments, or the state to which the coder is set when
    /// calling `clear`.
    #[pyo3(text_signature = "(self)")]
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    /// Returns a copy of the compressed data.
    ///
    /// You'll almost always want to call this method without arguments (which will default to
    /// `unseal=False`). See below for an explanation of the advanced use case with argument
    /// `unseal=True`.
    ///
    /// You will typically only want to call this method at the very end of your encoding task,
    /// i.e., once you've encoded the *entire* message. There is usually no need to call this method
    /// after encoding each symbol or other portion of your message. The encoders in `constriction`
    /// *accumulate* compressed data in an internal buffer, and encoding (semantically) *appends* to
    /// this buffer.
    ///
    /// That said, calling `get_compressed` has no side effects, so you *can* call `get_compressed`,
    /// then continue to encode more symbols, and then call `get_compressed` again. The first call
    /// of `get_compressed` will have no effect on the return value of the second call of
    /// `get_compressed`.
    ///
    /// The return value is a rank-1 numpy array of `dtype=np.uint32`. You can write it to a file by
    /// calling `to_file` on it, but we recommend to convert it into an architecture-independent
    /// byte order first:
    ///
    /// ```python
    /// import sys
    ///
    /// encoder = constriction.stream.stack.AnsCoder()
    /// # ... encode some message (skipped here) ...
    /// compressed = encoder.get_compressed() # returns a numpy array.
    /// if sys.byteorder != 'little':
    ///     # Let's save data in little-endian byte order by convention.
    ///     compressed.byteswap(inplace=True)
    /// compressed.tofile('compressed-file.bin')
    ///
    /// # At a later point, you might want to read and decode the file:
    /// compressed = np.fromfile('compressed-file.bin', dtype=np.uint32)
    /// if sys.byteorder != 'little':
    ///     # Restore native byte order before passing it to `constriction`.
    ///     compressed.byteswap(inplace=True)
    /// decoder = constriction.stream.stack.AnsCoder(compressed)
    /// # ... decode the message (skipped here) ...
    /// ```    
    ///
    /// ## Explanation of the optional argument `unseal`
    ///
    /// The optional argument `unseal` of this method is the counterpart to the optional argument
    /// `seal` of the constructor. Calling `.get_compressed(unseal=True)` tells the ANS coder that
    /// you expect it to be in a "sealed" state and instructs it to reverse the "sealing" operation.
    /// An ANS coder is in a sealed state if its encapsulated compressed data ends in a single "1"
    /// word. Calling the constructor of `AnsCoder` with argument `seal=True` constructs a coder
    /// that is guaranteed to be in a sealed state because the constructor will append a single "1"
    /// word to the provided `compressed` data. This sealing/unsealing operation makes sure that any
    /// trailing zero words are conserved since an `AnsCoder` would otherwise truncate them.
    ///
    /// Note that calling `.get_compressed(unseal=True)` fails if the coder is not in a "sealed"
    /// state.
    #[pyo3(text_signature = "(self, unseal=False)")]
    pub fn get_compressed<'p>(
        &mut self,
        py: Python<'p>,
        unseal: Option<bool>,
    ) -> PyResult<&'p PyArray1<u32>> {
        if unseal == Some(true) {
            let binary = self.inner.get_binary().map_err(|_|
                pyo3::exceptions::PyAssertionError::new_err(
                    "Cannot unseal compressed data because it doesn't fit into integer number of words. Did you create the encoder with `seal=True` and restore its original state?",
                ))?;
            Ok(PyArray1::from_slice(py, &binary))
        } else {
            Ok(PyArray1::from_slice(
                py,
                &self.inner.get_compressed().unwrap_infallible(),
            ))
        }
    }

    /// Encodes one or more symbols, appending them to the encapsulated compressed data.
    ///
    /// This method can be called in 3 different ways:
    ///
    /// ## Option 1: encode_reverse(symbol, model)
    ///
    /// Encodes a *single* symbol with a concrete (i.e., fully parameterized) entropy model; the
    /// suffix "_reverse" of the method name has no significance when called this way.
    ///
    /// For optimal computational efficiency, don't use this option in a loop if you can instead
    /// use one of the two alternative options below.
    ///
    /// For example:
    ///
    /// ```python
    /// # Define a concrete categorical entropy model over the (implied)
    /// # alphabet {0, 1, 2}:
    /// probabilities = np.array([0.1, 0.6, 0.3], dtype=np.float64)
    /// model = constriction.stream.model.Categorical(probabilities)
    ///
    /// # Encode a single symbol with this entropy model:
    /// coder = constriction.stream.stack.AnsCoder()
    /// coder.encode_reverse(2, model) # Encodes the symbol `2`.
    /// # ... then encode some more symbols ...
    /// ```
    ///
    /// ## Option 2: encode_reverse(symbols, model)
    ///
    /// Encodes multiple i.i.d. symbols, i.e., all symbols in the rank-1 array `symbols` will be
    /// encoded with the same concrete (i.e., fully parameterized) entropy model. The symbols are
    /// encoded in *reverse* order so that subsequent decoding will retrieve them in forward order
    /// (see [module-level example](#example)).
    ///
    /// For example:
    ///
    /// ```python
    /// # Use the same concrete entropy model as in the previous example:
    /// probabilities = np.array([0.1, 0.6, 0.3], dtype=np.float64)
    /// model = constriction.stream.model.Categorical(probabilities)
    ///
    /// # Encode an example message using the above `model` for all symbols:
    /// symbols = np.array([0, 2, 1, 2, 0, 2, 0, 2, 1], dtype=np.int32)
    /// coder = constriction.stream.stack.AnsCoder()
    /// coder.encode_reverse(symbols, model)
    /// print(coder.get_compressed()) # (prints: [1276728145, 172])
    /// ```
    ///
    /// ## Option 3: encode_reverse(symbols, model_family, params1, params2, ...)
    ///
    /// Encodes multiple symbols, using the same *family* of entropy models (e.g., categorical or
    /// quantized Gaussian) for all symbols, but with different model parameters for each symbol;
    /// here, each `paramsX` argument is an array of the same length as `symbols`. The number of
    /// required `paramsX` arguments and their shapes and `dtype`s depend on the model family. The
    /// symbols are encoded in *reverse* order so that subsequent decoding will retrieve them in
    /// forward order (see [module-level example](#example)). But the mapping between symbols and
    /// model parameters is as you'd expect it to be (i.e., `symbols[i]` gets encoded with model
    /// parameters `params1[i]`, `params2[i]`, and so on, where `i` counts backwards).
    ///
    /// For example, the
    /// [`QuantizedGaussian`](model.html#constriction.stream.model.QuantizedGaussian) model family
    /// expects two rank-1 model parameters of dtype `np.float64`, which specify the mean and
    /// standard deviation for each entropy model:
    ///
    /// ```python
    /// # Define a generic quantized Gaussian distribution for all integers
    /// # in the range from -100 to 100 (both ends inclusive):
    /// model_family = constriction.stream.model.QuantizedGaussian(-100, 100)
    ///    
    /// # Specify the model parameters for each symbol:
    /// means = np.array([10.3, -4.7, 20.5], dtype=np.float64)
    /// stds  = np.array([ 5.2, 24.2,  3.1], dtype=np.float64)
    ///    
    /// # Encode an example message:
    /// # (needs `len(symbols) == len(means) == len(stds)`)
    /// symbols = np.array([12, -13, 25], dtype=np.int32)
    /// coder = constriction.stream.stack.AnsCoder()
    /// coder.encode_reverse(symbols, model_family, means, stds)
    /// print(coder.get_compressed()) # (prints: [597775281, 3])
    /// ```
    ///
    /// By contrast, the [`Categorical`](model.html#constriction.stream.model.Categorical) model
    /// family expects a single rank-2 model parameter where the i'th row lists the
    /// probabilities for each possible value of the i'th symbol:
    ///
    /// ```python
    /// # Define 2 categorical models over the alphabet {0, 1, 2, 3, 4}:
    /// probabilities = np.array(
    ///     [[0.1, 0.2, 0.3, 0.1, 0.3],  # (for symbols[0])
    ///      [0.3, 0.2, 0.2, 0.2, 0.1]], # (for symbols[1])
    ///     dtype=np.float64)
    /// model_family = constriction.stream.model.Categorical()
    ///
    /// # Encode 2 symbols (needs `len(symbols) == probabilities.shape[0]`):
    /// symbols = np.array([3, 1], dtype=np.int32)
    /// coder = constriction.stream.stack.AnsCoder()
    /// coder.encode_reverse(symbols, model_family, probabilities)
    /// print(coder.get_compressed()) # (prints: [45298483])
    /// ```
    #[pyo3(signature = (symbols, model, *params), text_signature = "(self, symbols, model, *optional_model_params)")]
    pub fn encode_reverse(
        &mut self,
        py: Python<'_>,
        symbols: &PyAny,
        model: &Model,
        params: &PyTuple,
    ) -> PyResult<()> {
        if let Ok(symbol) = symbols.extract::<i32>() {
            if !params.is_empty() {
                return Err(pyo3::exceptions::PyAttributeError::new_err(
                    "To encode a single symbol, use a concrete model, i.e., pass the\n\
                    model parameters directly to the constructor of the model and not to the\n\
                    `encode` method of the entropy coder. Delaying the specification of model\n\
                    parameters until calling `encode_reverse` is only useful if you want to encode
                    several symbols in a row with individual model parameters for each symbol. If\n\
                    this is what you're trying to do then the `symbols` argument should be a numpy\n\
                    array, not a scalar.",
                ));
            }
            return model.0.as_parameterized(py, &mut |model| {
                self.inner
                    .encode_symbol(symbol, EncoderDecoderModel(model))?;
                Ok(())
            });
        }

        // Don't use an `else` branch here because, if the following `extract` fails, the returned
        // error message is actually pretty user friendly.
        let symbols = symbols.extract::<PyReadonlyArray1<'_, i32>>()?;
        let symbols = symbols.as_array();

        if params.is_empty() {
            model.0.as_parameterized(py, &mut |model| {
                self.inner
                    .encode_iid_symbols_reverse(symbols, EncoderDecoderModel(model))?;
                Ok(())
            })?;
        } else {
            if symbols.len() != model.0.len(&params[0])? {
                return Err(pyo3::exceptions::PyAttributeError::new_err(
                    "`symbols` argument has wrong length.",
                ));
            }
            let mut symbol_iter = symbols.iter().rev();
            model.0.parameterize(py, params, true, &mut |model| {
                let symbol = symbol_iter.next().expect("TODO");
                self.inner
                    .encode_symbol(*symbol, EncoderDecoderModel(model))?;
                Ok(())
            })?;
        }

        Ok(())
    }

    /// Decodes one or more symbols, consuming them from the encapsulated compressed data.
    ///
    /// This method can be called in 3 different ways:
    ///
    /// ## Option 1: decode(model)
    ///
    /// Decodes a *single* symbol with a concrete (i.e., fully parameterized) entropy model and
    /// returns the decoded symbol; (for optimal computational efficiency, don't use this option in
    /// a loop if you can instead use one of the two alternative options below.)
    ///
    /// For example:
    ///
    /// ```python
    /// # Define a concrete categorical entropy model over the (implied)
    /// # alphabet {0, 1, 2}:
    /// probabilities = np.array([0.1, 0.6, 0.3], dtype=np.float64)
    /// model = constriction.stream.model.Categorical(probabilities)
    ///
    /// # Decode a single symbol from some example compressed data:
    /// compressed = np.array([636697421, 6848946], dtype=np.uint32)
    /// coder = constriction.stream.stack.AnsCoder(compressed)
    /// symbol = coder.decode(model)
    /// print(symbol) # (prints: 2)
    /// # ... then decode some more symbols ...
    /// ```
    ///
    /// ## Option 2: decode(model, amt) [where `amt` is an integer]
    ///
    /// Decodes `amt` i.i.d. symbols using the same concrete (i.e., fully parametrized) entropy
    /// model for each symbol, and returns the decoded symbols as a rank-1 numpy array with
    /// `dtype=np.int32` and length `amt`;
    ///
    /// For example:
    ///
    /// ```python
    /// # Use the same concrete entropy model as in the previous example:
    /// probabilities = np.array([0.1, 0.6, 0.3], dtype=np.float64)
    /// model = constriction.stream.model.Categorical(probabilities)
    ///
    /// # Decode 9 symbols from some example compressed data, using the
    /// # same (fixed) entropy model defined above for all symbols:
    /// compressed = np.array([636697421, 6848946], dtype=np.uint32)
    /// coder = constriction.stream.stack.AnsCoder(compressed)
    /// symbols = coder.decode(model, 9)
    /// print(symbols) # (prints: [2, 0, 0, 1, 2, 2, 1, 2, 2])
    /// ```
    ///
    /// ## Option 3: decode(model_family, params1, params2, ...)
    ///
    /// Decodes multiple symbols, using the same *family* of entropy models (e.g., categorical or
    /// quantized Gaussian) for all symbols, but with different model parameters for each symbol,
    /// and returns the decoded symbols as a rank-1 numpy array with `dtype=np.int32`; here, all
    /// `paramsX` arguments are arrays of equal length (the number of symbols to be decoded). The
    /// number of required `paramsX` arguments and their shapes and `dtype`s depend on the model
    /// family.
    ///
    /// For example, the
    /// [`QuantizedGaussian`](model.html#constriction.stream.model.QuantizedGaussian) model family
    /// expects two rank-1 model parameters of dtype `np.float64`, which specify the mean and
    /// standard deviation for each entropy model:
    ///
    /// ```python
    /// # Define a generic quantized Gaussian distribution for all integers
    /// # in the range from -100 to 100 (both ends inclusive):
    /// model_family = constriction.stream.model.QuantizedGaussian(-100, 100)
    ///
    /// # Specify the model parameters for each symbol:
    /// means = np.array([10.3, -4.7, 20.5], dtype=np.float64)
    /// stds  = np.array([ 5.2, 24.2,  3.1], dtype=np.float64)
    ///
    /// # Decode a message from some example compressed data:
    /// compressed = np.array([597775281, 3], dtype=np.uint32)
    /// coder = constriction.stream.stack.AnsCoder(compressed)
    /// symbols = coder.decode(model_family, means, stds)
    /// print(symbols) # (prints: [12, -13, 25])
    /// ```
    ///
    /// By contrast, the [`Categorical`](model.html#constriction.stream.model.Categorical) model
    /// family expects a single rank-2 model parameter where the i'th row lists the
    /// probabilities for each possible value of the i'th symbol:
    ///
    /// ```python
    /// # Define 2 categorical models over the alphabet {0, 1, 2, 3, 4}:
    /// probabilities = np.array(
    ///     [[0.1, 0.2, 0.3, 0.1, 0.3],  # (for first decoded symbol)
    ///      [0.3, 0.2, 0.2, 0.2, 0.1]], # (for second decoded symbol)
    ///     dtype=np.float64)
    /// model_family = constriction.stream.model.Categorical()
    ///
    /// # Decode 2 symbols:
    /// compressed = np.array([2142112014, 31], dtype=np.uint32)
    /// coder = constriction.stream.stack.AnsCoder(compressed)
    /// symbols = coder.decode(model_family, probabilities)
    /// print(symbols) # (prints: [3, 1])
    /// ```
    #[pyo3(signature = (model, *params), text_signature = "(self, model, *optional_amt_or_model_params)")]
    pub fn decode(
        &mut self,
        py: Python<'_>,
        model: &Model,
        params: &PyTuple,
    ) -> PyResult<PyObject> {
        match params.len() {
            0 => {
                let mut symbol = 0;
                model.0.as_parameterized(py, &mut |model| {
                    symbol = self
                        .inner
                        .decode_symbol(EncoderDecoderModel(model))
                        .unwrap_infallible();
                    Ok(())
                })?;
                return Ok(symbol.to_object(py));
            }
            1 => {
                if let Ok(amt) = usize::extract(params.as_slice()[0]) {
                    let mut symbols = Vec::with_capacity(amt);
                    model.0.as_parameterized(py, &mut |model| {
                        for symbol in self
                            .inner
                            .decode_iid_symbols(amt, EncoderDecoderModel(model))
                        {
                            symbols.push(symbol.unwrap_infallible());
                        }
                        Ok(())
                    })?;
                    return Ok(PyArray1::from_iter(py, symbols).to_object(py));
                }
            }
            _ => {} // Fall through to code below.
        };

        let mut symbols = Vec::with_capacity(model.0.len(&params[0])?);
        model.0.parameterize(py, params, false, &mut |model| {
            let symbol = self
                .inner
                .decode_symbol(EncoderDecoderModel(model))
                .unwrap_infallible();
            symbols.push(symbol);
            Ok(())
        })?;

        Ok(PyArray1::from_vec(py, symbols).to_object(py))
    }

    /// Creates a deep copy of the coder and returns it.
    ///
    /// The returned copy will initially encapsulate the identical compressed data as the
    /// original coder, but the two coders can be used independently without influencing
    /// other.
    #[pyo3(text_signature = "(self)")]
    pub fn clone(&self) -> Self {
        Clone::clone(self)
    }
}
