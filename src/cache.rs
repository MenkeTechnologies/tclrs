//! The compiled-chunk cache, keyed by source text.
//!
//! A Tcl script can build a script at run time and evaluate it, and `eval` in a
//! loop evaluates the same text on every pass. Parsing and lowering that text
//! once is the whole reason tclrs compiles rather than interprets, so the text
//! is the cache key: identical source is identical bytecode, whatever produced
//! it.
//!
//! The key is the source text and nothing else. Nothing outside the text can
//! change what it lowers to — the compiler reads no interpreter state, and the
//! variables a chunk touches are bound to slots by name, not by value — so
//! there is no context to fold into the key and no way for an entry to go
//! stale within a process.
//!
//! Entries are held as `Arc<Chunk>` and cloned into the VM per run, because
//! `fusevm::VM::new` takes the chunk by value. That clone copies the op vector,
//! the constant pool and the name table; it does not parse, resolve a variable
//! name, or lay out a jump.

use std::collections::HashMap;
use std::sync::Arc;

use fusevm::Chunk;

use crate::runtime::TclError;

/// How many compiled scripts one interpreter keeps.
pub const DEFAULT_CAPACITY: usize = 1024;

/// Compiled scripts, keyed by their source text.
#[derive(Debug)]
pub struct ChunkCache {
    entries: HashMap<String, Arc<Chunk>>,
    capacity: usize,
    hits: u64,
    misses: u64,
}

impl ChunkCache {
    /// A cache holding up to [`DEFAULT_CAPACITY`] scripts.
    pub fn new() -> Self {
        Self::with_capacity(DEFAULT_CAPACITY)
    }

    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            entries: HashMap::new(),
            capacity: capacity.max(1),
            hits: 0,
            misses: 0,
        }
    }

    /// The chunk for `src`, compiling it on the first request.
    ///
    /// A source that fails to compile is not stored: the failure is reported on
    /// every attempt, and a diagnostic is not worth a cache slot.
    pub fn compile(&mut self, src: &str) -> Result<Arc<Chunk>, TclError> {
        if let Some(chunk) = self.entries.get(src) {
            self.hits += 1;
            return Ok(Arc::clone(chunk));
        }
        self.misses += 1;
        // An inline `rust { ... }` block is not Tcl and never reaches the
        // parser: it is rewritten into a command first. The cache is keyed by
        // the source as written, so the rewrite is paid once per distinct
        // script, like the parse and the lowering it is part of.
        let rewritten = crate::rust_ffi::desugar(src);
        let script = crate::parser::parse(&rewritten).map_err(|e| TclError {
            msg: e.msg,
            line: Some(e.line),
            code: crate::runtime::TCL_ERROR,
            level: 0,
        })?;
        let chunk = Arc::new(crate::compiler::compile(&script).map_err(|e| TclError {
            msg: e.msg,
            line: Some(e.line),
            code: crate::runtime::TCL_ERROR,
            level: 0,
        })?);
        // At capacity the whole cache is dropped rather than one entry chosen.
        // An `eval` loop reuses a handful of sources and never reaches the
        // limit; a script that reaches it is generating fresh text every pass,
        // where no eviction order would have kept the useful entry either.
        if self.entries.len() >= self.capacity {
            self.entries.clear();
        }
        self.entries.insert(src.to_string(), Arc::clone(&chunk));
        Ok(chunk)
    }

    /// How many compiled scripts are held.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// `(hits, misses)` — one miss per compilation performed.
    pub fn stats(&self) -> (u64, u64) {
        (self.hits, self.misses)
    }

    /// Drop every entry, keeping the counters.
    pub fn clear(&mut self) {
        self.entries.clear();
    }
}

impl Default for ChunkCache {
    fn default() -> Self {
        Self::new()
    }
}
