# Deferred / Explored Ideas — aatxe diff + synth optimization

## Wins (committed)
1. **Remove `format!` in `is_ignored`** — `path.split('/').any(|c| c == dir)` avoids per-check String allocation. filter_huge -54%.
2. **Sort-first dedup in `synth.rs`** — sort by (file, line) before dedup, then scan backwards only within same-file window. Eliminates O(N²) cross-file comparisons. synth_dedup_256 -47%.
3. **Byte-level hunk counting in `parse_one_file`** — split metadata scan (line iterator) from hunk counting (fast byte scan for `\n+`/`\n-`). Avoids per-line iterator overhead for 99% of lines. parse_huge -5.3%.
4. **mem::replace with capacity in `chunk_for_review_owned`** — `std::mem::take` resets String capacity to 0, causing reallocations for every subsequent chunk. Using `std::mem::replace(&mut cur_body, String::with_capacity(...))` preserves capacity. chunk_huge -51%.
5. **Sequential parse threshold** — thread spawn overhead (~100µs/thread) dominates for <100 files. Force `n_threads = 1` when `pieces.len() < 100`. parse_huge additional improvement to ~15.54s.
6. **Prompt builder `write!`/`writeln!`** — replace `format!` + `push_str` with direct writes to output String. Eliminates per-line temporary String allocations.
7. **Exact body capacity** — `with_capacity(11 + len)` instead of `13 + len` for "diff --git " prefix.
8. **Pre-size thread-local result Vecs** — `collect()` on `FilterMap` has conservative size hint. Explicit `with_capacity(chunk.len())` + `extend` avoids reallocations in parallel parse workers.

## Discarded (reverted)
1. **Buffer reuse in `title_jaccard`** — lifetime issues with `&mut Vec<&str>` across nested loops in `dedup_and_rank`. Borrow checker rejects because `f.title` borrow outlives the inner loop.
2. **`find(dir)` + boundary checks in `is_ignored`** — `find()` is 44% slower than `split('/').any()` for directory matching on short paths.
3. **`[].concat()` for body construction** — regressed vs `with_capacity` + `push_str`.
4. **Manual `push_str` for `raised_by` merge** — `format!("{a}+{b}")` is faster for small strings (~20 bytes) than manual `with_capacity` + `push_str`.
5. **Inline `tokenise`/`title_jaccard`** — caused 8.5% synth regression (code bloat / register pressure).
6. **Pre-size `chunks` Vec and larger `cur_body` capacity** — no measurable impact; lost in noise.
7. **Pre-size `cur_files` to 32** — reallocations are tiny, no benchmark impact.
8. **Pre-size output Vec in sequential parse path** — no measurable improvement.

## Remaining Bottlenecks
- **Parse body allocation dominates** — each file copies its body into a new String. For 2000 files × ~50KB = 100MB of copying. Cannot eliminate without changing `ParsedFile::body` from `String` to `Cow`/`&str`, which would ripple through the public API.
- **`title_jaccard` allocates 2 Vecs per comparison** — for ~200 comparisons in the benchmark, that's 400 small heap allocations. Buffer reuse was blocked by borrow checker.
- **`chunk_for_review` borrowed variant still clones bodies** — not measured by benchmark, but production callers using the borrowed API pay full clone cost.

## Future Ideas (if API constraints relax)
1. **Change `ParsedFile::body` to `Arc<str>` or `Cow<'static, str>`** — avoids per-file body copies when the original diff string can be kept alive.
2. **Bump allocator for body strings** — allocate one big arena per parse call, store body offsets instead of owned Strings.
3. **Batch chunk body building** — pre-compute chunk boundaries with a prefix sum, then build chunk bodies in parallel.
4. **Buffer reuse for `title_jaccard`** — pass `&mut Vec<&str>` buffers from the caller (requires restructuring `dedup_and_rank` to avoid borrow checker issues).
