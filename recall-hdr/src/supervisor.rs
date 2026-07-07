//! Capture-engine supervisor: spawns/monitors `recall.exe --json`, restart
//! backoff, log-only protocol parsing, orderly shutdown (display restore is
//! never blocked by the child). Implemented in Wave 2 by a dedicated agent.
