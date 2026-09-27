//! Сущности, хранилище (SQLite), конфигурация и модели экранов
//! (Архитектура/nuncio - Приложение (Rust и Slint).md, Данные и хранилище.md).
//! Домен не знает, что за сетью стоит Go: он видит трейт `Backend`.

pub mod backend;
pub mod config;
pub mod login;
pub mod model;
pub mod phone;
pub mod store;

pub use backend::{Backend, BackendError, Next};
