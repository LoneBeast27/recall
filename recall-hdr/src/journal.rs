//! Crash-safety state journal (%APPDATA%\recall-hdr\state.json): records the
//! pre-mutation display baseline so any exit path — including a hard kill — can
//! restore it (startup restore / --restore). Implemented in Wave 1 by a
//! dedicated agent — stub reserves the module.
