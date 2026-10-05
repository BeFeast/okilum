# Discussion send backend evidence

Backend implementation for [#244](https://git.oklabs.uk/BeFeast/tessera/issues/244),
following the [frozen send/recovery contract](ai-brain-discussion-send-recovery.md).
The shell, frozen old-writer round trip and isolated native acceptance are separate
checks; the backend tests below do not claim those outcomes.

## Implementation boundary

`chat_send` classifies the original operation while holding the backend owner
mutex. It reads Runner pending writes and only the requested SourceStore journal,
validates the original proposed bytes and receipt, and returns an existing
operation before consulting current goal, actor, provider or source state. Only
`Admission::NewlyCommitted` starts the provider. `chat_send_get` calls the same
read-only lookup directly and cannot enter preparation or flush canonical writes.

The client UUID is used only for that turn's initial conversation SourceWrite.
Later stream/final writes retain generated UUIDs. The existing pending-write and
RecoveryRecord formats remain unchanged; the adjacent `discussion_send` metadata
is retained in original proposed bytes. No second backend outbox, operation scan,
new persistent field in Runner/Application state or public fault-injection feature
was introduced.

Missing records return `unknown`; malformed, foreign, contradictory or oversized
records return a recovery error. Neither proves that a request was never accepted.
A projected receipt proves a canonical projection, not provider delivery. After
record deletion, the client must remain lookup-only for the original operation.

## Bounded reader fixture

The source reader limits the actual read to the caller's ceiling plus one byte,
under the existing writer lock. It neither scans nor retries other operations.
The Discussion ceiling is **268,500,996 serialized bytes**:

- Proposed bytes: 64 MiB, encoded as 89,478,488 Base64 bytes.
- Preimage bytes: 128 MiB, encoded as 178,956,972 Base64 bytes.
- Bounded JSON/path metadata reserve: 65,536 bytes.

The real compact `RecoveryRecord` serializer, with both payload ceilings and a
4096-byte path consisting of U+0001, produces **268,485,317 bytes**. Its metadata
overhead is **49,857 bytes**; the worst-escaped path appears in both request and
receipt. This fixture establishes a conservative serialization/read ceiling; its
synthetic payload and path are not claimed to be a valid maximum Discussion
conversation. The test exercises actual normal-size read, exact serialized cap,
cap plus one byte and divergent-history overflow through the real bounded reader.

The adapter separately checks decoded proposed/preimage sizes, request and
retained manual paths (4096 UTF-8 bytes each), actor (4096 UTF-8 bytes), 32 sorted
unique paths, existing context limits, original request digest and provider body
hash. Every serialized operation result, including `unknown`, must fit 64 KiB.
Overflow is an explicit recovery error, never a truncated or missing result.

## Fault evidence by layer

These are complementary tests of real seams. They are **not one process-level
end-to-end crash test across every phase**.

| Boundary or condition | Test layer and observation |
| --- | --- |
| Before Runner persist | Runner checkpoint hook: no retained operation, lookup unknown; checkpoint failure has no terminal rejection marker. |
| After Runner persist, before SourceStore intent | Runner hook: original pending SourceWrite and IDs; lookup/replay do not flush. Normal restart recovery completes the canonical write without provider preparation. |
| After SourceStore intent, before replacement | Core `write_with_hook` at IntentSaved/Prepared: original caller UUID, proposed bytes and preimage survive; bounded reread preserves journal and canonical bytes. |
| After replacement, before receipt | Core Replaced hook: original proposed bytes are canonical; retained intent has no receipt; bounded lookup performs no reconciliation. |
| Receipt before provider spawn | Real Runner post-write interruption through service dispatch: typed recovery error, validated projected lookup, zero listener connections; direct connection is the listener's positive control. |
| Lost service ACK after admission | Real TCP client sends a request then closes without reading. Server dispatch survives, lookup returns original IDs and actual HTTP provider count stays unchanged. |
| Concurrent same UUID | Two threads enter service dispatch together, return identical operation results and cause exactly one HTTP request. A distinct UUID with identical payload causes a second request. |
| Source edits/deletion, canonical metadata replacement/file deletion | Real Runner and service lookup/replay retain original IDs and immutable source receipt, including after Runner reopen and with no configured provider. |
| Invalid evidence | Foreign writes, corrupt JSON/Base64, wrong owner/payload, UUID/brain/revision mismatch, contradictory receipt/conflict, duplicate pending UUIDs, pending/journal disagreement and invalid pending target/revision bindings return explicit errors without rewriting evidence. |
| Retained conflict | Valid conflict returns projection_conflict with no source receipt and never re-enters preparation. |
| Terminal rejection | Digest-valid new input rejected before checkpoint carries the exact bound recorded:false marker. Invalid digest and checkpoint errors do not carry it. |

Shared digest fixtures cover CRLF and non-ASCII/control/combining text. Service
tests compare the SHA-256 of each actual captured HTTP request body with the
returned provider request hash. Existing legacy Discussion tests remain part of
the full backend suite.
