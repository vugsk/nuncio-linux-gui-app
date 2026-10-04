//! Сущности, хранилище (SQLite), конфигурация и модели экранов
//! (Архитектура/nuncio - Приложение (Rust и Slint).md, Данные и хранилище.md).
//! Домен не знает, что за сетью стоит Go: он видит трейт `Backend`.

pub mod attach;
pub mod backend;
pub mod chat;
pub mod chatlist;
pub mod chatstore;
pub mod config;
pub mod history;
pub mod login;
pub mod media;
pub mod model;
pub mod notify;
pub mod outbox;
pub mod phone;
pub mod settings;
pub mod store;
pub mod timefmt;

pub use backend::{Backend, BackendError, Next};
