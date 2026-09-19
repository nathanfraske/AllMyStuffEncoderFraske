# Frozen terminal baseline

These snapshots and literal expectations were frozen from
`ee9cc150f4bdc3fd49a547b563f0381dae3685f5` before reading the extracted
implementation. `manifest.json` records 18 complete input identities, nine
verbatim source snapshots and four independent literal-vector files. The
snapshot ranges use one-based inclusive source line numbers; their bytes come
directly from Git blobs, without formatting or header changes.

The real terminal and stub snapshots include their complete original source.
The runtime snapshot preserves the node's first-registration-wins registry and
exact missing-runtime panic. Byte queues, mesh terminal callers, IPC dispatch,
protocol row and media-frame shapes remain available for boundary comparison.
The existing node test excerpt also records that the apparent sequence-dedup
test constructs `Mesh`, while the runtime spawn test does not.

The literals deliberately preserve existing edge cases: the `u16::MAX` resize
sentinel fallback, zero-capacity scrollback, whole-chunk viewer eviction,
empty-chunk framing, watcher tokens, host/stub detach differences and the lazy
runtime lookup. They also distinguish actual generation and bridge behavior
from stronger comments in the old source. This extraction is not a repair of
those behaviors.

No Rust, PTY, shell candidate or test was executed to create these snapshots or
expectations. Existing test counts describe source inventory: two pure cases,
eleven Unix PTY cases, and one Windows PTY case. Actual baseline and extracted
results belong in the extraction evidence report. These nested files are data;
do not turn the archived original tests into additional automatic test targets.
