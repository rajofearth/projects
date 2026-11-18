//! Where updates show: a line at the foot of the sidebar while one downloads or waits, and an
//! Updates group in Settings with the switch, the version, what changed and the buttons.

use rune::{col, row, El, Tx};
use rune_kit::{Button, ButtonVariant, ControlSize, Kit, Switch};

use crate::icons;
use crate::ui::{bar, data, label, Ink, S1, S2, S3};
use crate::update::machine::Activity;
use crate::State;

/// Installs the downloaded update and quits, so the new version takes the window.
pub fn restart(s: &mut State, tx: &mut Tx) {
    match s.updates.install_now() {
        Ok(()) => tx.quit(),
        Err(e) => s.toast_error(format!("Couldn't install the update: {e}")),
    }
}

/// The sidebar's update line, or nothing when there is nothing to say.
pub fn sidebar_line(s: &State, ink: &Ink, k: &Kit) -> Option<El> {
    let st = &s.updates.state;
    if let Some(ready) = &st.ready {
        return Some(
            row()
                .key("update-ready")
                .w_full()
                .items_center()
                .gap(S2)
                .child(label(format!("Update {}", ready.version), 12.0, 500.0, ink.fg).grow(1.0).min_w(0.0).truncate())
                .child(
                    Button::new("Restart")
                        .variant(ButtonVariant::Ghost)
                        .control_size(ControlSize::Small)
                        .help("Install the update and reopen Projects")
                        .on_press_tx(restart)
                        .build(k)
                        .key("update-restart"),
                ),
        );
    }
    if let Activity::Downloading { version, done, total } = &st.activity {
        let f = if *total > 0 { *done as f32 / *total as f32 } else { 0.0 };
        return Some(
            col()
                .key("update-downloading")
                .w_full()
                .gap(S1)
                .child(label(format!("Downloading {version}"), 12.0, 400.0, ink.muted))
                .child(bar(f, ink.faint, ink)),
        );
    }
    None
}

fn line(what: &str, value: El, ink: &Ink) -> El {
    row().w_full().h(28.0).items_center().gap(S3).child(label(what, 13.0, 400.0, ink.row).w(120.0)).child(value)
}

/// The Updates group's body in Settings.
pub fn settings(s: &State, ink: &Ink, k: &Kit) -> El {
    let st = &s.updates.state;
    let off = st.disabled.is_some();
    let status_color = if st.error.is_some() && st.ready.is_none() { ink.red } else if st.ready.is_some() { ink.fg } else { ink.muted };
    let checked = st.checked_at.map(|t| crate::fmt::secs_ago(crate::saved::now_unix() - t));

    let mut body = col()
        .w_full()
        .gap(S2)
        .child(
            row()
                .w_full()
                .items_center()
                .gap(S3)
                .child(
                    col()
                        .grow(1.0)
                        .gap(2.0)
                        .child(label("Download updates automatically", 13.0, 400.0, ink.row))
                        .child(
                            label("Checks GitHub when Projects opens and every 4 hours. You choose when to restart.", 12.0, 400.0, ink.faint)
                                .wrap()
                                .w_full(),
                        ),
                )
                .child(
                    Switch::new(s.saved.auto_update && !off)
                        .label("Download updates automatically")
                        .hide_label()
                        .disabled(off)
                        .on_toggle(|s: &mut State| {
                            let on = !s.saved.auto_update;
                            s.saved.set_auto_update(on);
                        })
                        .build(k)
                        .key("auto-update"),
                ),
        )
        .child(line("Installed", data(format!("{}", st.current), 12.0, ink.fg), ink))
        .child(line("Status", label(st.headline(), 13.0, 400.0, status_color).grow(1.0).min_w(0.0).wrap(), ink))
        .children(checked.filter(|_| !off).map(|c| line("Last checked", data(c, 12.0, ink.muted), ink)));

    if let Some(ready) = st.ready.as_ref().filter(|r| !r.notes.is_empty()) {
        body = body.child(
            col()
                .w_full()
                .gap(S1)
                .pt(S1)
                .child(label(format!("What's new in {}", ready.version), 12.0, 400.0, ink.faint))
                .children(ready.notes.iter().map(|n| {
                    row()
                        .w_full()
                        .gap(S2)
                        .child(label("·", 13.0, 400.0, ink.faint))
                        .child(label(n.clone(), 13.0, 400.0, ink.muted).grow(1.0).min_w(0.0).wrap())
                })),
        );
    }

    if !off {
        let buttons = row()
            .w_full()
            .gap(S2)
            .pt(S2)
            .child(
                Button::new(if st.busy() { "Checking" } else { "Check now" })
                    .variant(ButtonVariant::Neutral)
                    .icon(&icons::REFRESH)
                    .disabled(st.busy())
                    .on_press(|s: &mut State| {
                        let waker = s.waker.clone();
                        s.updates.check_now(waker.as_ref());
                    })
                    .build(k)
                    .key("update-check"),
            )
            .children(st.ready.as_ref().map(|_| {
                Button::new("Restart to update")
                    .variant(ButtonVariant::Submit)
                    .icon(&icons::DOWNLOAD)
                    .on_press_tx(restart)
                    .build(k)
                    .key("update-install")
            }));
        body = body.child(buttons);
        if st.ready.is_some() {
            body = body.child(label("If you close Projects instead, the update installs the next time it opens.", 12.0, 400.0, ink.faint).wrap().w_full());
        }
    }
    body
}
