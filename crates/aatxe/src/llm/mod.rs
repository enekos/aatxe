//! LLM backends for the agent council.
//!
//! Every backend produces the same `Finding[]` JSON the council pipeline
//! consumes; they differ only in transport and tool access:
//!
//! * [`pi_proxy`] / [`claude_code`] — shell out to a local agent CLI that
//!   runs the model + a read-only repo tool loop. Both share the spawn,
//!   timeout, and output-sanitisation plumbing in [`subprocess_llm`].
//! * [`gemini_http`] — a direct blocking HTTP client; no tools, pre-packed
//!   prompt only.
//! * [`stub_client`] — deterministic canned responses for offline tests + CI.

pub mod claude_code;
pub mod gemini_http;
pub mod pi_proxy;
pub mod stub_client;
pub mod subprocess_llm;
