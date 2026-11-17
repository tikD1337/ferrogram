//! Wires explicit UI file requests to the bounded native shell worker.
use std::rc::Rc;
use tg_core::{CommandError, Event};
pub(crate) fn attach(ui: &tg_ui::Ui) -> Option<Rc<tg_platform::Files>> {
    let sink = ui.event_sink();
    let files = Rc::new(
        tg_platform::Files::start(move |success| {
            if !success {
                sink.post(Event::FileOperationFailed);
            }
        })
        .ok()?,
    );
    let requests = files.clone();
    ui.on_file(move |request| {
        requests.request(request).map_err(|_| CommandError {
            code: -4,
            message: "The file worker is busy or unavailable.".into(),
        })
    });
    Some(files)
}
