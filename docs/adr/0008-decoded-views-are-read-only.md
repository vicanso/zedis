# 8. Decoded views are read-only; the hex view is the only write path

Date: 2026-09-12 · Status: accepted

## Context

A string value (and, since #141, a hash field, list item or set member)
is shown decoded: gzip, zstd, Snappy and LZ4 are unpacked, MessagePack,
BSON, pickle, Java serialization, JWT and PHP `serialize()` are rendered
as pretty JSON. `detect_and_decode` in `states/server/string.rs` folds
every such result into `DataFormat::Preview`; the bytes editor treats
only `Text` and `Json` as editable, so a decoded view is read-only and
the hex view is the one place the stored bytes can be changed. The rule
is stated in FEATURES.md in a sentence; the reasons were not written
down anywhere, and the question "why can't I edit what I can read?"
keeps coming back — most recently as a proposal to write compressed
values back through their decompressed text.

The formats split in two:

- **Compression wrappers** (gzip, zstd, Snappy framed, LZ4 block) do have
  a faithful inverse: decompress, edit, recompress with the variant the
  detector pinned. The payload inside is usually text or JSON, which is
  what a user would actually want to change.
- **Serialization formats** have no honest inverse. The JSON shown is a
  projection: MessagePack loses integer-vs-float, signed-vs-unsigned,
  `bin`-vs-`str`, extension types (its timestamps, application ext
  types) and non-string map keys; pickle, Java and BSON lose their type
  systems the same way. Re-encoding from that JSON produces a value the
  application that wrote it will typically fail to deserialize — a
  field expecting `bin` receives `str`, an integer key comes back as a
  string, a timestamp is gone — and the failure shows up far from Zedis
  as "Zedis corrupted my value".

Writing compressed values back is feasible but cannot be offered
uniformly. It would be gated three ways: the outer format must be one
the detector knows, the inner payload must be text, and the preview must
not have been truncated (`format_text` clips long values to
`max_truncate_length` and marks them `Preview`, so a write from a
clipped preview would store a clipped payload). Each gate is invisible
in the editor — a gzip-wrapped JSON and a MessagePack-decoded JSON look
identical there — and the truncation gate is not even stable per key: the
same value flips between editable and not as it grows or as the setting
changes.

The issue tracker has asked for decoding in more places (#105, #141) and
never for writing a decoded value back. Compressed values are written by
application code, and the hex view is useless for them in practice, so
they have been effectively unwritable in Zedis all along without a
complaint.

## Decision

Decoded views stay read-only, for compression wrappers and serialization
formats alike. The hex view remains the only way to change the bytes of a
value that is not plain text or JSON. Collection elements follow the same
rule through `element.rs`: decoded for display, the stored bytes on every
write.

The distinction is kept in this record, not in the product: a rule with
one clause ("decoded means read-only") is learned once, while a rule with
three invisible gates is re-guessed on every key. Serialization formats
are closed for good — there is nothing to build. Compression write-back
is deferred, not rejected: it is reopened by a user request that comes
with a concrete scenario, and any implementation must refuse to save
from a truncated preview, recompress with the exact variant detected, and
say in the UI that the bytes will differ from the original.

## Consequences

- A new decoder (a format added to `DataFormat` and `detect_and_decode`)
  is display-only by definition; it does not need to answer the question
  of how it writes back, and must not quietly answer it for itself.
- FEATURES.md keeps the one-sentence rule; this ADR is where the reasons
  live, so the write-back proposal is not re-derived from scratch.
- A user who needs to change a compressed value today deletes the key
  and lets the application repopulate it, or writes the plain payload
  from the terminal — both are the honest paths until a scenario says
  otherwise.
