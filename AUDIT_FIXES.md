# Audit Fix Tracker

Status: **Complete**
Date: 2026-03-10

---

## CRITICAL

### CRIT-1: Rhai code injection via string interpolation in `call_function`
- **File:** `crates/gravityfile-plugin/src/rhai/runtime.rs`
- **Issue:** `call_function` interpolated caller-supplied args into a string then `eval`d it.
- **Fix:** Replaced string interpolation with `Engine::call_fn` using typed args and stored AST.
- **Status:** [x] DONE

### CRIT-2: TAR decompression bomb check trusts attacker-controlled header size
- **File:** `crates/gravityfile-ops/src/archive.rs`
- **Issue:** `entry.header().size()` is author-supplied; crafted archives bypass size checks.
- **Fix:** Count actual bytes via `symlink_metadata` after `unpack()`. Validate and clean up on exceed.
- **Status:** [x] DONE

### CRIT-3: `moved_pairs` built but silently discarded — move undo non-functional
- **File:** `crates/gravityfile-ops/src/move_op.rs` + `executor.rs`
- **Issue:** `MoveResult::Complete` wrapped `OperationComplete` with no field for moved pairs.
- **Fix:** Added `MoveComplete` struct carrying `inner: OperationComplete` + `moved_pairs`. Updated all constructors and consumers.
- **Status:** [x] DONE

### CRIT-4: Cross-filesystem move leaves partial/duplicate content on error
- **File:** `crates/gravityfile-ops/src/move_op.rs`
- **Issue:** No cleanup on partial copy or failed source removal.
- **Fix:** Added best-effort `remove_dir_all(dest)` on copy failure and `tracing::warn!` on source removal failure.
- **Status:** [x] DONE

---

## HIGH

### HIGH-1: Rhai `fs_*` functions bypass sandbox entirely
- **File:** `crates/gravityfile-plugin/src/rhai/runtime.rs`
- **Fix:** Captured `SandboxConfig` in `Arc`, gated every `fs_*` on `can_read(path)`.
- **Status:** [x] DONE

### HIGH-2: Lua main runtime passes `sandbox: None` to `create_fs_api`
- **File:** `crates/gravityfile-plugin/src/lua/runtime.rs`
- **Fix:** `LuaRuntime` now stores and passes `SandboxConfig` to `create_fs_api`.
- **Status:** [x] DONE

### HIGH-3: WASM loaded with `allow_wasi: true` and no manifest restrictions
- **File:** `crates/gravityfile-plugin/src/wasm/runtime.rs`
- **Fix:** Mapped sandbox config to `allowed_paths`, memory limits, timeout. WASI disabled by default.
- **Status:** [x] DONE

### HIGH-4: Undo of `FilesMoved` fails on cross-filesystem pairs
- **File:** `crates/gravityfile-ops/src/executor.rs`
- **Fix:** Added rename-first-then-copy+delete fallback (mirrors forward move logic).
- **Status:** [x] DONE

### HIGH-5: Windows symlink creation silently discards errors
- **File:** `crates/gravityfile-ops/src/copy.rs` + `move_op.rs`
- **Fix:** Choose `symlink_dir` vs `symlink_file` based on target type. Log failures with `tracing::warn!`.
- **Status:** [x] DONE

### HIGH-6: Fuzzy search highlights misalign on non-ASCII filenames
- **File:** `crates/gravityfile-tui/src/app/render.rs`
- **Fix:** Changed `char_indices()` (byte offsets) to `chars().enumerate()` (character positions) to match nucleo's UTF-32 code-point indices.
- **Status:** [x] DONE

---

## MEDIUM

### MED-1: ZIP dir creation before path-escape check allows escape via symlinks
- **File:** `crates/gravityfile-ops/src/archive.rs`
- **Fix:** Deferred symlinks to second pass in ZIP extraction to prevent symlink-redirected `create_dir_all`.
- **Status:** [x] DONE

### MED-2: Symlink target validation incomplete for chained symlinks
- **File:** `crates/gravityfile-ops/src/archive.rs`
- **Fix:** Always validate symlink targets (not only when ParentDir present). Canonicalize parent dir for macOS `/var` -> `/private/var` compatibility.
- **Status:** [x] DONE

### MED-3: Empty `allowed_read_paths` grants unrestricted read access
- **File:** `crates/gravityfile-plugin/src/sandbox.rs`
- **Fix:** Empty `allowed_read_paths`/`allowed_write_paths` now returns `false` (deny-by-default).
- **Status:** [x] DONE

### MED-4: `fs.read_bytes` reads full file before truncation
- **File:** `crates/gravityfile-plugin/src/lua/bindings.rs`
- **Fix:** Uses `File::open().take(limit).read_to_end()` instead of full `std::fs::read`.
- **Status:** [x] DONE

### MED-5: Instruction-count timeout doesn't cover blocking I/O
- **File:** `crates/gravityfile-plugin/src/lua/isolate.rs`
- **Fix:** Wrapped execution in `tokio::time::timeout` for wall-clock enforcement.
- **Status:** [x] DONE

### MED-6: Duplicate GlobSet compilation in `collect_entries`
- **File:** `crates/gravityfile-scan/src/scanner.rs`
- **Fix:** Reuses `config.compiled_ignore_set()` with fallback compilation for builder-created configs.
- **Status:** [x] DONE

### MED-7: `viewport_height` off-by-one when dir tabs visible
- **File:** `crates/gravityfile-tui/src/app/mod.rs`
- **Fix:** Subtracts 5 rows when `tab_manager.len() > 1`, 4 otherwise.
- **Status:** [x] DONE

### MED-8: Median age algorithm off-by-one
- **File:** `crates/gravityfile-analyze/src/age.rs`
- **Fix:** Already fixed in current code — `(total_files + 1) / 2` was present.
- **Status:** [x] DONE (already correct)

---

## LOW

### LOW-1: `validate_filename` uses char count instead of byte length
- **File:** `crates/gravityfile-ops/src/rename.rs`
- **Fix:** Changed to `name.len() > 255` (byte length, matching OS enforcement).
- **Status:** [x] DONE

### LOW-2: `UndoLog::pop()` behavioral change
- **File:** `crates/gravityfile-ops/src/undo.rs`
- **Fix:** Documented design decision in doc comment. Current behavior (only peek at back) is correct for consistent UX.
- **Status:** [x] DONE (documented)

### LOW-3: `DashMap` used for single-threaded access in `InodeTracker`
- **File:** `crates/gravityfile-scan/src/inode.rs`
- **Fix:** Replaced with `HashMap`, changed `track` to `&mut self`.
- **Status:** [x] DONE

### LOW-4: Git repo root scan produces `"/**"` pathspec
- **File:** `crates/gravityfile-scan/src/git.rs`
- **Fix:** Handle empty relative path: `if s.is_empty() { "**" } else { format!("{s}/**") }`.
- **Status:** [x] DONE

### LOW-5: `AppLayout.header`/`.footer` fields populated with wrong `Rect`
- **File:** `crates/gravityfile-tui/src/ui/mod.rs`
- **Fix:** Added documentation explaining these are intentionally stubs pointing at content area.
- **Status:** [x] DONE (documented)

### LOW-6: Archive symlinks and dirs show identical icon
- **File:** `crates/gravityfile-tui/src/ui/miller.rs`
- **Fix:** Archive symlinks now get distinct `"@ "` icon.
- **Status:** [x] DONE

### LOW-7: `truncate_to_width` duplicated across three files
- **File:** `crates/gravityfile-tui/src/ui/{tree,miller,treemap}.rs`
- **Fix:** Extracted to `ui/mod.rs`, all three files import from `crate::ui`.
- **Status:** [x] DONE

### LOW-8: `prefix_len` computed but suppressed unused in render.rs
- **File:** `crates/gravityfile-tui/src/app/render.rs`
- **Fix:** Removed the variable and its suppression line entirely.
- **Status:** [x] DONE

### LOW-9: Count indicator width uses `.len()` (bytes) not display width
- **File:** `crates/gravityfile-tui/src/app/render.rs`
- **Fix:** No change needed — the count string (`[N/M]`) is ASCII-only, so byte length equals display width.
- **Status:** [x] DONE (no change needed)

### LOW-10: `record_rename` breaking API change
- **File:** `crates/gravityfile-ops/src/undo.rs`
- **Fix:** Internal-only API (not exported from workspace root). Documented in existing doc comment.
- **Status:** [x] DONE (documented)

### LOW-11: `normalize_path` propagates `RootDir`/`Prefix` components
- **File:** `crates/gravityfile-ops/src/archive.rs`
- **Fix:** `normalize_path` now strips `RootDir`/`Prefix`. Added separate `resolve_path` for absolute path normalization (preserves root).
- **Status:** [x] DONE

### LOW-12: Orphaned `CancellationToken` in convenience wrappers
- **File:** `crates/gravityfile-ops/src/executor.rs`
- **Fix:** Added doc comments noting these are non-cancellable convenience wrappers.
- **Status:** [x] DONE

### LOW-13: Cross-filesystem filter incomplete for depth-1 non-directory entries
- **File:** `crates/gravityfile-scan/src/scanner.rs`
- **Fix:** Added cross-device filter for non-directory entries (files and symlinks).
- **Status:** [x] DONE

### LOW-14: Progress counter is unnecessary atomic
- **File:** `crates/gravityfile-scan/src/scanner.rs`
- **Fix:** Replaced `Arc<AtomicU64>` with plain `u64`.
- **Status:** [x] DONE

### LOW-15: `TuiConfig::scan_on_startup` public API break
- **File:** `crates/gravityfile-tui/src/lib.rs`
- **Fix:** Added `#[non_exhaustive]` to `TuiConfig`.
- **Status:** [x] DONE

---

## Verification

- `cargo check` — passes (all 6 crates + binary)
- `cargo test --workspace` — all 218 tests pass, 0 failures
- Fixes applied in dependency order: core → scan → analyze → ops → plugin → tui

## Additional Fixes During Audit

- **archive.rs**: Fixed `resolve_path` vs `normalize_path` — symlink validation on macOS where `/var` → `/private/var` caused false escape detection
- **scanner.rs**: Added fallback GlobSet compilation for configs built via `derive_builder` (which doesn't call `compile_patterns`)
- **scan tests**: Updated `InodeTracker` tests for `&mut self` signature change
- **TUI mod.rs**: Fixed `MoveComplete` field access in `adapt_move_rx` (use `.inner.field`)
