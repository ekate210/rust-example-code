# Content service excerpts (Digital Asset Management)

This folder contains selected Rust modules from a digital asset management
platform I worked on. These files are **excerpts**, not a standalone crate. Shared types, config,
utilities, and internal libraries stay under NDA, so imports such as
`common_rust`, `crate::types`, and `crate::config` resolve only in the original
service. The goal here is to show real production structure: traits, adapters,
factories, error handling, retries, and provider-specific API work.

## Auth provider (`auth_provider/`)

Abstraction over identity providers so organization and user operations stay
the same while the backend can be Auth0 or Clerk. This supported a full
Auth0 → Clerk migration (thousands of users, ID format changes, zero-downtime
data transfer) and invitation / membership flows wired into a custom
authorization service.

| File | Role |
|------|------|
| `auth_provider_trait.rs` | Shared async trait: org users, invites, user CRUD, org metadata, sign-in tokens. Default methods return forbidden so unused ops fail closed. `mockall` for unit tests. |
| `auth0_service.rs` | Auth0 Management API: M2M token, Lucene user search, org invites/cancel, membership delete, profile update. |
| `clerk_service.rs` | Clerk Backend API: paginated org members + pending invites, user/org lifecycle, metadata merge, structured error mapping (validation vs retryable). Includes focused unit tests. |
| `../auth_provider_factory.rs` | Resolves `Auth0` / `Clerk` to `dyn AuthProviderTrait`; optional mock injection for tests. |

## Storage service (`storage_service/`)

Adapter layer for multi-storage content: ingest, head metadata, purge,
pre-signed upload/download, stage between adapters, URI strip/reconstruct, and
adapter lifecycle. Callers pick an implementation by adapter id / type via the
factory—the same pattern as auth providers.

| File | Role |
|------|------|
| `storage_service_trait.rs` | Common storage contract plus DTOs (`GenerateUploadUrlOptions`, `StageItemResult`, etc.). |
| `aws_s3_service.rs` | S3: region-cached clients, transient retry, object tagging from metadata, pre-signed GET/PUT, cross-adapter upload/stage. |
| `aws_efs_service.rs` | POSIX/EFS paths: head via filesystem metadata, purge with empty-dir cleanup, stage by downloading from another adapter. |
| `cdn_service.rs` | CDN (Bunny.net): tokenized URLs, pull-zone create/update/delete, purge, image optimizer query params, idempotent API retries with jitter. |
| `channel_manager_service.rs` | Channel / social-style URIs via internal gRPC, then HEAD (or range GET) against the resolved URL with auth headers. |
| `../storage_service_factory.rs` | Routes adapter id prefixes / enums to the right `StorageServiceTrait`; global lazy factory + mock constructor for tests. |
