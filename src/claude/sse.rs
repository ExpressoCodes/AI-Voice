// SSE parsing was previously used by the Anthropic REST API client.
// With the switch to CLI subprocess backends, SSE is no longer needed here.
// If an HTTP backend is added in the future, move SSE parsing into that
// backend module.
