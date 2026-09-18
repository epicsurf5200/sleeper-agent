// C ABI for the sleeper-agent Rust core.
//
// Kept deliberately small: everything the app needs crosses as JSON through
// sa_request, so adding a feature never means adding a symbol here.
// Mirrors ios/sa-ffi/src/lib.rs — keep the two in step.

#ifndef SA_FFI_H
#define SA_FFI_H

#include <stddef.h>

#ifdef __cplusplus
extern "C" {
#endif

/// Opaque engine handle.
typedef struct SAEngine SAEngine;

/// Called with the JSON reply. The string is freed as soon as this returns,
/// so copy anything you need to keep.
typedef void (*SAResponseCallback)(void *ctx, const char *response_json);

/// Create the engine. Both paths must be writable directories inside the app
/// sandbox. Returns NULL on failure.
SAEngine *sa_engine_new(const char *config_dir, const char *cache_dir);

/// Release an engine created by sa_engine_new.
void sa_engine_free(SAEngine *engine);

/// Run a request. Returns immediately; `cb` fires on a worker thread.
/// `ctx` must stay valid until `cb` has run.
void sa_request(SAEngine *engine,
                const char *request_json,
                void *ctx,
                SAResponseCallback cb);

/// Core version string. Statically allocated — do not free.
const char *sa_version(void);

#ifdef __cplusplus
}
#endif

#endif /* SA_FFI_H */
