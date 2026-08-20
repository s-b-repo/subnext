//! DCR — Dynamic Context Runtime.
//!
//! An implementation of the specification in `docs/`: unlimited external state
//! plus a small, planned working set.
//!
//! ```
//! use dcr::Dcr;
//!
//! let mut rt = Dcr::new(800);
//! rt.ingest("The server ip is 10.0.4.12.", None).unwrap();
//! let answer = rt.ask("what is the server ip?", None);
//! println!("{} ({} tokens)", answer.text, answer.tokens);
//! ```
//!
//! The modules map one-to-one onto the wiki:
//!
//! | module | spec page |
//! |---|---|
//! | [`spans`] | L0 immutable raw store (`R_t`) |
//! | [`ladder`] | representation ladder L0–L3 |
//! | [`nodes`] | typed state schema |
//! | [`graph`] | dynamic memory graph (`S_t`, `E_t`) |
//! | [`execute`] | execution layer + memoisation (`C_t`) |
//! | [`indexer`] | state indexer / node extraction |
//! | [`index`] | hybrid retrieval over text *and* state |
//! | [`policy`] | runtime decision policy |
//! | [`budget`] | attention-budget knapsack (`B_attention`) |
//! | [`planner`] | relevance planner (`k + r`) |
//! | [`speculation`] | speculative context (`tau`, prefetch) |
//! | [`telemetry`] | escalation / staleness / prefetch metrics |
//! | [`runtime`] | the facade tying it together |

pub mod bench;
pub mod budget;
pub mod demo;
pub mod embed;
pub mod execute;
pub mod graph;
pub mod ids;
pub mod index;
pub mod indexer;
pub mod json;
pub mod ladder;
pub mod llm;
pub mod nodes;
pub mod planner;
pub mod policy;
pub mod runtime;
pub mod spans;
pub mod speculation;
pub mod summarize;
pub mod telemetry;
pub mod text;
pub mod tokens;

pub use budget::{Allocation, Candidate, Choice, solve};
pub use execute::ExecutionLayer;
pub use graph::{DcrError, ExplainPath, MemoryGraph};
pub use index::HybridIndex;
pub use indexer::{HeuristicExtractor, IndexResult, StateIndexer};
pub use ladder::{Ladder, Level};
pub use llm::{CommandReasoner, LocalReasoner, Reasoner};
pub use nodes::{Edge, EdgeType, Kind, Node, NodeIdx, Status};
pub use planner::{ActiveContext, RelevancePlanner, Weights};
pub use policy::{DecisionPolicy, QueryType};
pub use runtime::{Answer, Dcr};
pub use spans::{RawStore, Span};
pub use speculation::{Predictor, Speculator};
pub use telemetry::Telemetry;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
