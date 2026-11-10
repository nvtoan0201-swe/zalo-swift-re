//! Feature modules not yet ported route here: snapshots are recorded (shape only) so nothing is lost silently.

use std::rc::Rc;

use olaz_main::MainProcess;
use serde_json::{Map, Value};

use crate::engine::ZaloEngine;

pub fn handle(engine: &ZaloEngine, key: &str, state: &Map<String, Value>) {
    match key {
        "notification" => engine.handle_notification(state),
        "compose" => engine.handle_compose(state),
        "pins" => engine.handle_pins(state),
        "convActions" => engine.handle_conversation_actions(state),
        "contacts" => engine.handle_contacts(state),
        "poll" => engine.handle_poll(state),
        "zcloud" => engine.handle_cloud(state),
        "transfer" => engine.handle_transfer(state),
        "transferAvailable" => engine.handle_transfer_availability(state),
        _ => olaz_core::log::write(format!("feature snapshot {key} (not handled yet)")),
    }
}

pub fn install_modules(main: &Rc<MainProcess>, engine: &Rc<ZaloEngine>) {
    let loader = engine.media_loader().unwrap_or_else(|| {
        let ua = olaz_core::fingerprint::user_agent();
        olaz_media::MediaLoader::new(&engine.paths.cache(), vec![engine.paths.user_data(), engine.paths.cache()], &ua)
    });
    let downloads = olaz_media::download::DownloadManager::new(loader, engine.paths.cache().join("download-tmp"));
    main.modules.borrow_mut().push(Rc::new(olaz_media::download::DownloadModule(downloads)));
    main.modules.borrow_mut().push(Rc::new(crate::notifications::NotifierModule(engine.notifier.clone())));
}

pub fn drop_message_prompts(engine: &ZaloEngine) {
    engine.drop_message_prompts();
}

pub fn note_poll_messages(engine: &ZaloEngine, chat: &Value) {
    use crate::models::J;
    let conv = chat.s("convId");
    let incoming: Vec<_> = chat.arr("messages").iter().filter_map(|m| crate::models::ChatMessage::from_json(m, &conv)).collect();
    engine.note_poll_messages(&incoming);
}
