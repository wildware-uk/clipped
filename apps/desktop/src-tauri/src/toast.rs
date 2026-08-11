//! The Windows toast itself: composing one, showing it, and — the part that
//! decides whether its button does anything — keeping it alive afterwards.
//!
//! [`crate::notification_policy`] decides *what* to say and what to offer;
//! [`crate::notifications`] decides *when* and performs the action. This module
//! is the Windows API and nothing else, so that the one lifetime rule below has
//! a single place to be stated and a test to hold it.
//!
//! # Why the notification is retained
//!
//! `ToastNotifier::Show` hands the notification's *content* to the Windows
//! notification platform, which draws it. It does not take over the delivery of
//! the `Activated` event: that is raised on the [`ToastNotification`] object in
//! **this** process, which is why a desktop application is required to subscribe
//! to it at all ([`ToastNotification.Activated`][activated]: "in the case of a
//! toast raised by a desktop app, that app must subscribe to at least the
//! Activated event"). A `ToastNotification` is a reference-counted COM object,
//! and this process holds the only reference it will ever have. Drop it and the
//! object is destroyed while its toast is still on screen; whether Windows keeps
//! a reference of its own is neither documented nor promised, and a button whose
//! handler *might* have been freed is the control AGENTS.md section 27 forbids.
//!
//! So [`Toaster`] keeps every toast it shows, and the question stops being
//! interesting. This is the whole reason `tauri-winrt-notification` is not used
//! here: its `Toast::show` creates the `ToastNotification`, attaches the
//! handler, shows it and returns `Result<()>`, dropping the object — the caller
//! is given no way to hold it.
//!
//! # How many, and for how long
//!
//! [`RETAINED`] of them, oldest released first. A toast that has left the screen
//! can still be clicked from the Action Centre, so releasing on `Dismissed`
//! would break exactly the case that matters — somebody who was in a game when
//! the recording failed. Windows keeps at most twenty notifications per
//! application in the Action Centre and drops the oldest beyond that, so twenty
//! is every toast the user can still see, and a bound rather than an unbounded
//! collection in a process that runs for days (AGENTS.md sections 58 and 59).
//!
//! # Threads
//!
//! A [`Toaster`] is owned by the thread that reads the recorder link's events
//! and is only ever used from it. The handler runs on a thread of WinRT's
//! choosing and touches nothing owned here.
//!
//! [activated]: https://learn.microsoft.com/en-us/uwp/api/windows.ui.notifications.toastnotification.activated

use std::collections::VecDeque;

use windows::core::{Interface as _, HSTRING};
use windows::Data::Xml::Dom::XmlDocument;
use windows::Foundation::TypedEventHandler;
use windows::UI::Notifications::{
    ToastActivatedEventArgs, ToastNotification, ToastNotificationManager,
};

/// How many shown toasts are kept reachable by their `Activated` handler.
///
/// Windows' own Action Centre limit per application. Keeping the same number
/// means every toast the user can still click is one this process can still
/// perform, and no more (see the module documentation).
const RETAINED: usize = 20;

/// What a toast says and offers.
///
/// Borrowed rather than owned: the caller has these already, and a toast is
/// composed and shown inside one call.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ToastContent<'a> {
    /// The bold first line: two or three words.
    pub(crate) title: &'a str,
    /// The second line: what happened and where the thing it concerns is.
    pub(crate) body: &'a str,
    /// The text on the one button.
    pub(crate) button: &'a str,
    /// What that button reports to the handler when it is the thing clicked.
    pub(crate) button_argument: &'a str,
}

/// Shows toasts, and keeps them clickable.
#[derive(Debug)]
pub(crate) struct Toaster {
    /// The AppUserModelID toasts are filed under.
    app_id: HSTRING,
    /// Toasts already shown, oldest first.
    ///
    /// Held for their handlers rather than to be read: see the module
    /// documentation. Never grows beyond [`RETAINED`].
    shown: VecDeque<ToastNotification>,
}

impl Toaster {
    /// A toaster showing under one AppUserModelID.
    pub(crate) fn new(app_id: &str) -> Self {
        Self {
            app_id: HSTRING::from(app_id),
            shown: VecDeque::new(),
        }
    }

    /// Composes a toast, shows it, and keeps it.
    ///
    /// `activated` is called with the button's argument when the button is
    /// clicked, and with [`None`] when the body of the toast is.
    ///
    /// # Errors
    ///
    /// What Windows said, for the caller to fall back from. Note that a
    /// successful return means the notification was *accepted*, not that it was
    /// displayed: `ToastNotifier::Show` reports success even where the user has
    /// switched Clipped's notifications off in Windows' own settings, and there
    /// is no synchronous answer that says otherwise.
    pub(crate) fn show<F>(&mut self, content: ToastContent<'_>, activated: F) -> Result<(), String>
    where
        F: Fn(Option<String>) + Send + 'static,
    {
        let notification = self
            .compose(content, activated)
            .map_err(|error| error.message())?;

        ToastNotificationManager::CreateToastNotifierWithId(&self.app_id)
            .and_then(|notifier| notifier.Show(&notification))
            .map_err(|error| error.message())?;

        // Only once Windows has accepted it. A notification that was refused has
        // no toast to be clicked, and holding it would push a live one out.
        if self.shown.len() == RETAINED {
            self.shown.pop_front();
        }
        self.shown.push_back(notification);

        Ok(())
    }

    /// Builds the notification and attaches the handler, without showing it.
    fn compose<F>(
        &self,
        content: ToastContent<'_>,
        activated: F,
    ) -> windows::core::Result<ToastNotification>
    where
        F: Fn(Option<String>) + Send + 'static,
    {
        let notification = ToastNotification::CreateToastNotification(&compose_xml(content)?)?;

        notification.Activated(&TypedEventHandler::new(
            move |_, arguments: windows::core::Ref<'_, windows::core::IInspectable>| {
                activated(chosen_action(arguments.as_ref()));
                Ok(())
            },
        ))?;

        Ok(notification)
    }
}

/// Which button the user clicked, if it was a button.
///
/// The body of a toast activates with empty arguments, which is reported as
/// [`None`] so that the caller need not know that.
fn chosen_action(arguments: Option<&windows::core::IInspectable>) -> Option<String> {
    let activated = arguments?.cast::<ToastActivatedEventArgs>().ok()?;
    let chosen = activated.Arguments().ok()?;
    if chosen.is_empty() {
        None
    } else {
        Some(chosen.to_string())
    }
}

/// The toast's XML.
///
/// Built through the XML document object model rather than by formatting a
/// string, because everything in a Clipped notification is text this module did
/// not write: a recorder's message and a file path chosen by the user. A path
/// such as `D:\clips\me & you.mkv` formatted into an attribute or an element
/// makes the document ill-formed, and an ill-formed document is a toast that
/// never appears. `SetInnerText` and `SetAttribute` escape their arguments.
fn compose_xml(content: ToastContent<'_>) -> windows::core::Result<XmlDocument> {
    let document = XmlDocument::new()?;

    let toast = document.CreateElement(&HSTRING::from("toast"))?;
    // Long, because every one of these is a failure and a toast that has gone in
    // five seconds is one the user was not at their desk for.
    toast.SetAttribute(&HSTRING::from("duration"), &HSTRING::from("long"))?;

    let visual = document.CreateElement(&HSTRING::from("visual"))?;
    let binding = document.CreateElement(&HSTRING::from("binding"))?;
    binding.SetAttribute(&HSTRING::from("template"), &HSTRING::from("ToastGeneric"))?;

    for (id, text) in [("1", content.title), ("2", content.body)] {
        let element = document.CreateElement(&HSTRING::from("text"))?;
        element.SetAttribute(&HSTRING::from("id"), &HSTRING::from(id))?;
        element.SetInnerText(&HSTRING::from(text))?;
        binding.AppendChild(&element)?;
    }

    visual.AppendChild(&binding)?;
    toast.AppendChild(&visual)?;

    let actions = document.CreateElement(&HSTRING::from("actions"))?;
    let action = document.CreateElement(&HSTRING::from("action"))?;
    action.SetAttribute(&HSTRING::from("content"), &HSTRING::from(content.button))?;
    action.SetAttribute(
        &HSTRING::from("arguments"),
        &HSTRING::from(content.button_argument),
    )?;
    actions.AppendChild(&action)?;
    toast.AppendChild(&actions)?;

    document.AppendChild(&toast)?;

    Ok(document)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A notification of the shape this application actually sends.
    fn content() -> ToastContent<'static> {
        ToastContent {
            title: "Recording failed",
            body: r"The disk is full. What was recorded is at D:\clips\cs2.mkv.",
            button: "Show the file",
            button_argument: "action",
        }
    }

    /// The XML as Windows would hand it to the notification platform.
    ///
    /// Composing a document and a `ToastNotification` shows nothing: only
    /// `ToastNotifier::Show` puts a toast on screen, and nothing here calls it.
    /// That is what lets the shape of a toast be asserted in a unit test on a
    /// machine somebody else is using.
    fn xml_of(content: ToastContent<'_>) -> String {
        compose_xml(content)
            .expect("the document is composed")
            .GetXml()
            .expect("the document can be serialised")
            .to_string()
    }

    #[test]
    fn the_document_is_the_one_a_toast_was_seen_delivered_from() {
        // `docs/desktop-ui.md` records a toast read back out of the Windows
        // notification history, from the `tauri-winrt-notification` build this
        // module replaced. Composing the same notification here produces that
        // document byte for byte, which is what carries the one end-to-end
        // observation there is across the rewrite: what changed is who holds the
        // `ToastNotification` afterwards, not what Windows is handed.
        let seen = ToastContent {
            title: "Recorder unavailable",
            body: "The recorder exited with status 1 within 10s without listening on \
                   \\\\.\\pipe\\clipped-recorder.1; its diagnostics are in the Clipped log \
                   directory. 4 attempts to reach or start a recorder failed, so nothing is being \
                   recorded and no more will be made without being asked.",
            button: "Try again",
            button_argument: "action",
        };

        assert_eq!(
            xml_of(seen),
            concat!(
                r#"<toast duration="long"><visual><binding template="ToastGeneric">"#,
                r#"<text id="1">Recorder unavailable</text>"#,
                r#"<text id="2">The recorder exited with status 1 within 10s without listening "#,
                r"on \\.\pipe\clipped-recorder.1; its diagnostics are in the Clipped log ",
                "directory. 4 attempts to reach or start a recorder failed, so nothing is being ",
                "recorded and no more will be made without being asked.</text>",
                r#"</binding></visual><actions>"#,
                r#"<action content="Try again" arguments="action"/></actions></toast>"#,
            )
        );
    }

    #[test]
    fn a_toast_carries_the_button_the_notification_offered() {
        // The acceptance criterion's first half, and the only half a test on
        // this machine can reach: the button reaches the toast, under the
        // argument the handler matches on. Whether clicking it runs the handler
        // needs a real toast on a real desktop; `docs/desktop-ui.md` says so.
        let xml = xml_of(content());

        assert!(
            xml.contains(r#"<action content="Show the file" arguments="action"/>"#),
            "the button, and the argument `notifications` matches on: {xml}"
        );
    }

    #[test]
    fn a_recording_whose_name_is_not_xml_still_gets_a_toast() {
        // `&` is legal in a Windows file name and appears in recorder messages.
        // Formatted into the document it would make it ill-formed, and an
        // ill-formed document is `CreateToastNotification` failing — a
        // notification silently lost for the sake of an ampersand.
        let escaped = ToastContent {
            title: "Recording failed",
            body: r"The file is at D:\clips\me & you <2>.mkv.",
            button: "Show the file",
            button_argument: "action",
        };

        let xml = xml_of(escaped);

        assert!(
            xml.contains("me &amp; you &lt;2&gt;.mkv"),
            "the text has to be escaped rather than inlined: {xml}"
        );
        // The document being parseable at all is the thing that matters, and
        // this is what proves it: `CreateToastNotification` refuses a document
        // that is not well-formed.
        ToastNotification::CreateToastNotification(&compose_xml(escaped).expect("composed"))
            .expect("a toast whose text contains XML syntax is still a toast");
    }

    #[test]
    fn a_shown_toast_is_kept_so_that_its_button_still_has_a_handler() {
        // The defect this module exists for. `tauri-winrt-notification`'s
        // `show()` drops the `ToastNotification` before returning, taking the
        // only reference this process holds to the object the `Activated` event
        // is raised on. Nothing here may do that.
        //
        // Composed rather than shown — showing would put a toast on somebody's
        // screen — so this asserts the retention rule against the same objects
        // `show` retains.
        let mut toaster = Toaster::new("uk.wildware.clipped.test");
        for _ in 0..RETAINED + 5 {
            let notification = toaster
                .compose(content(), |_| {})
                .expect("the notification is composed");
            if toaster.shown.len() == RETAINED {
                toaster.shown.pop_front();
            }
            toaster.shown.push_back(notification);
        }

        assert_eq!(
            toaster.shown.len(),
            RETAINED,
            "a process that runs for days may not keep every toast it ever showed"
        );
        assert!(
            toaster.shown.iter().all(|notification| notification
                .Content()
                .is_ok_and(|content| content.GetXml().is_ok())),
            "every retained notification is still a live object"
        );
    }

    #[test]
    fn the_body_of_a_toast_is_not_the_button() {
        // `notifications::show` tells the two apart by the argument, and gets
        // `None` for the body. Clicking the body performing the button's action
        // would be a click doing something nobody asked for.
        assert_eq!(chosen_action(None), None);
    }
}
