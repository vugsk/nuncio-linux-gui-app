//! Рекламные сообщения (условия API Telegram): в каналах и чатах с ботами.
//! Показываются одной строкой в конце ленты (решение пользователя — менее
//! навязчиво, чем в официальном клиенте), по щелчку раскрываются. Просмотр
//! отмечается, когда строка действительно попала в окно; щелчок по ссылке —
//! отдельно. «О рекламодателе» и «Скрыть» (жалоба с выбором причины).

use crate::chat::{ChatEffect, ChatModel, Pending};
use crate::model::{Command, CommandResult, Payload, PeerKind, Sponsored, SponsoredReportKind};

/// Выбор причины жалобы: заголовок и варианты (текст, значение для сервера).
pub type ReportChoice = (String, Vec<(String, Vec<u8>)>);

/// Реклама открытого чата и её состояние на экране.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SponsoredState {
    pub peer_id: i64,
    pub ad: Sponsored,
    /// Просмотр уже отправлен.
    pub viewed: bool,
    /// Строка раскрыта в карточку.
    pub expanded: bool,
    /// Открыто окно «О рекламодателе».
    pub info: bool,
    /// Выбор причины жалобы: заголовок и варианты.
    pub report: Option<ReportChoice>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SponsoredIntent {
    /// Строка попала в окно.
    Seen,
    /// Раскрыть или свернуть.
    Toggle,
    /// Щелчок по ссылке (её открывает интерфейс).
    Open,
    Info(bool),
    /// «Скрыть»: запросить причины.
    Hide,
    /// Выбрана причина.
    ReportPick(usize),
    ReportCancel,
}

impl ChatModel {
    /// Открыт чат: реклама есть только в каналах и у ботов.
    pub(crate) fn sponsored_chat_opened(&mut self, out: &mut Vec<ChatEffect>) {
        self.sponsored = None;
        let Some(peer_id) = self.open else { return };
        let wants = self.open_peer().is_some_and(|p| (p.kind == PeerKind::Channel && p.broadcast) || p.is_bot);
        if !wants {
            return;
        }
        if let Some(peer) = self.peer_ref(peer_id) {
            self.send(Command::GetSponsored { peer }, Pending::Sponsored { peer_id }, out);
        }
    }

    pub(crate) fn sponsored_result(&mut self, peer_id: i64, r: &CommandResult) {
        if self.open != Some(peer_id) {
            return;
        }
        if let (None, Payload::Sponsored(list)) = (&r.error, &r.payload) {
            self.sponsored = list.first().map(|ad| SponsoredState { peer_id, ad: ad.clone(), ..Default::default() });
        }
    }

    /// Реклама для показа: открытого чата и только у конца ленты.
    pub fn sponsored_here(&self) -> Option<&SponsoredState> {
        self.sponsored.as_ref().filter(|s| Some(s.peer_id) == self.open && self.window.end)
    }

    pub(crate) fn sponsored_intent(&mut self, i: SponsoredIntent, out: &mut Vec<ChatEffect>) {
        let Some(s) = self.sponsored.as_mut() else { return };
        let random_id = s.ad.random_id.clone();
        match i {
            SponsoredIntent::Seen => {
                if !s.viewed {
                    s.viewed = true;
                    self.send(Command::ViewSponsored { random_id }, Pending::Other, out);
                }
            }
            SponsoredIntent::Toggle => s.expanded = !s.expanded,
            SponsoredIntent::Open => self.send(Command::ClickSponsored { random_id }, Pending::Other, out),
            SponsoredIntent::Info(on) => s.info = on,
            SponsoredIntent::Hide => {
                if s.ad.can_report {
                    self.send(Command::ReportSponsored { random_id, option: Vec::new() }, Pending::SponsoredReport, out);
                } else {
                    // Пожаловаться нельзя — скрыть до следующего открытия чата.
                    self.sponsored = None;
                }
            }
            SponsoredIntent::ReportPick(n) => {
                let option = s.report.as_ref().and_then(|(_, o)| o.get(n)).map(|(_, b)| b.clone());
                s.report = None;
                if let Some(option) = option {
                    self.send(Command::ReportSponsored { random_id, option }, Pending::SponsoredReport, out);
                }
            }
            SponsoredIntent::ReportCancel => s.report = None,
        }
    }

    pub(crate) fn sponsored_report_result(&mut self, r: &CommandResult) {
        let ru = self.lang == "ru";
        match (&r.error, &r.payload) {
            (None, Payload::SponsoredReport { kind: SponsoredReportKind::Choose, title, options }) => {
                if let Some(s) = self.sponsored.as_mut() {
                    s.report = Some((title.clone(), options.clone()));
                }
            }
            (None, Payload::SponsoredReport { kind, .. }) => {
                self.sponsored = None;
                let t = match (kind, ru) {
                    (SponsoredReportKind::Hidden, true) => "Реклама скрыта",
                    (SponsoredReportKind::Hidden, false) => "Ads hidden",
                    (_, true) => "Жалоба отправлена",
                    (_, false) => "Report sent",
                };
                self.toast(t);
            }
            (Some(e), _) => {
                let t = crate::errors::error_text(e, self.lang == "ru");
                self.toast(&t);
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat::ChatIntent;
    use crate::model::{ErrorInfo, Event, Peer};

    fn model(bot: bool) -> ChatModel {
        let mut m = ChatModel::new(1, "ru", 1000);
        m.extra_peers.insert(10, Peer { id: 10, access_hash: 30, is_bot: bot, ..Default::default() });
        m
    }

    fn sends(out: &[ChatEffect]) -> Vec<(u64, Command)> {
        out.iter().filter_map(|e| if let ChatEffect::Send { id, cmd } = e { Some((*id, cmd.clone())) } else { None })
            .collect()
    }

    fn ad() -> Sponsored {
        Sponsored { random_id: vec![7], title: "Т".into(), can_report: true, ..Default::default() }
    }

    fn reply(id: u64, payload: Payload) -> Event {
        Event::Result(CommandResult { cmd_id: id, error: None, payload })
    }

    #[test]
    fn ads_only_for_bots_and_channels_view_once_report() {
        let mut m = model(false);
        let fx = m.intent(ChatIntent::OpenChat(10));
        assert!(!sends(&fx).iter().any(|(_, c)| matches!(c, Command::GetSponsored { .. })), "в личном чате рекламы нет");

        let mut m = model(true);
        let fx = m.intent(ChatIntent::OpenChat(10));
        let id = sends(&fx).into_iter().find(|(_, c)| matches!(c, Command::GetSponsored { .. })).expect("у бота — запрос").0;
        m.event(&reply(id, Payload::Sponsored(vec![ad()])));
        m.window.end = true;
        assert!(m.sponsored_here().is_some());
        let fx = m.intent(ChatIntent::Sponsored(SponsoredIntent::Seen));
        assert!(matches!(&sends(&fx)[..], [(_, Command::ViewSponsored { random_id })] if random_id == &vec![7]));
        assert!(sends(&m.intent(ChatIntent::Sponsored(SponsoredIntent::Seen))).is_empty(), "просмотр — один раз");
        // Скрыть: причины, выбор, принято — рекламы нет.
        let fx = m.intent(ChatIntent::Sponsored(SponsoredIntent::Hide));
        let (id, _) = sends(&fx)[0].clone();
        m.event(&reply(id, Payload::SponsoredReport { kind: SponsoredReportKind::Choose, title: "Почему?".into(),
            options: vec![("Не интересно".into(), vec![1])] }));
        assert!(m.sponsored.as_ref().unwrap().report.is_some());
        let fx = m.intent(ChatIntent::Sponsored(SponsoredIntent::ReportPick(0)));
        let (id, cmd) = sends(&fx)[0].clone();
        assert_eq!(cmd, Command::ReportSponsored { random_id: vec![7], option: vec![1] });
        m.event(&reply(id, Payload::SponsoredReport { kind: SponsoredReportKind::Reported, title: String::new(),
            options: vec![] }));
        assert!(m.sponsored.is_none());
        // Ошибка запроса — без рекламы, без падения.
        let mut m = model(true);
        let fx = m.intent(ChatIntent::OpenChat(10));
        let (id, _) = sends(&fx).into_iter().find(|(_, c)| matches!(c, Command::GetSponsored { .. })).unwrap();
        m.event(&Event::Result(CommandResult { cmd_id: id, error: Some(ErrorInfo { kind: crate::model::ErrorKind::Rpc, code: 400, text: "X".into(), retry_after: 0 }), payload: Payload::None }));
        assert!(m.sponsored.is_none());
    }
}
