//! Proves what a disabled `trace_frame!` does, as opposed to how fast it is.
//!
//! `hot_loop_cost.rs` measures the cost and finds it indistinguishable from
//! noise. This states the underlying property directly, and unlike a
//! measurement it cannot be flaky: the arguments are never evaluated, so no
//! amount of work inside them can end up on a capture thread.
//!
//! It is a separate binary from the measurement because installing a second
//! subscriber in a process forces `tracing` to abandon its cached
//! per-callsite decisions, which changes the very cost that file is measuring.

/// Records that it was called, and returns something a log field can hold.
fn count(evaluations: &mut u32) -> u32 {
    *evaluations += 1;
    *evaluations
}

#[test]
fn a_disabled_frame_trace_does_not_evaluate_its_arguments() {
    // The subscriber matters. It accepts `TRACE`, so a `trace_frame!` that had
    // lost its feature gate would evaluate its arguments here and the count
    // would be wrong. With no subscriber at all the test would pass either
    // way, because `tracing` itself skips the arguments of an event nothing is
    // listening to.
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_writer(std::io::sink)
        .finish();

    let mut evaluations = 0_u32;
    tracing::subscriber::with_default(subscriber, || {
        for index in 0..1_000_u64 {
            clipped_logging::trace_frame!(
                frame_index = index,
                expensive = count(&mut evaluations),
                "frame acquired"
            );
        }
    });

    let expected = if clipped_logging::FRAME_TRACING {
        1_000
    } else {
        0
    };
    assert_eq!(
        evaluations,
        expected,
        "trace_frame! evaluated its arguments {evaluations} times with the \
         frame-tracing feature {}",
        if clipped_logging::FRAME_TRACING {
            "on"
        } else {
            "off"
        }
    );
}
