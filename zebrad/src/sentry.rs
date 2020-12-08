use sentry::{
    integrations::backtrace::current_stacktrace,
    protocol::{Event, Exception, Mechanism},
};

pub fn panic_event_from<T>(msg: T) -> Event<'static>
where
    T: ToString,
{
    let exception = Exception {
        ty: "panic".into(),
        mechanism: Some(Mechanism {
            ty: "panic".into(),
            handled: Some(false),
            ..Default::default()
        }),
        value: Some(msg.to_string()),
        stacktrace: current_stacktrace(),
        ..Default::default()
    };

    Event {
        exception: vec![exception].into(),
        level: sentry::Level::Fatal,
        ..Default::default()
    }
}
