# Report compatibility fixtures

These manually specified fixtures freeze legacy raw scan and analysis JSON contracts.
The analysis examples predate completeness metadata. They are consumer examples, not captured user
data. They intentionally include byte counts above JavaScript's exact integer
range and timestamps/durations with nonzero nanoseconds.

Do not regenerate them from the current serializer or update them merely to
make a test pass. A format change must preserve these reads or document an
explicit migration/version boundary. The tests also exercise additive fields,
malformed known fields, and unknown completeness for legacy reports.

The scan fixture covers a directory and file, statistics, warnings, nanosecond
times, integer precision, and defaults for absent optional metadata and configuration.
It deliberately omits native names and Git status to protect legacy reads.
It does not exhaust every node variant, native-path encoding, or the
versioned CLI envelope. CLI integration tests separately cover envelope output,
strict-mode failures, and source scan warnings.
