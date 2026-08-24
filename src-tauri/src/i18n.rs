use std::collections::HashMap;

pub struct TrayI18n {
    strings: HashMap<&'static str, &'static str>,
}

impl TrayI18n {
    pub fn new(lang: &str) -> Self {
        let mut strings = HashMap::new();
        match lang {
            "pl" => {
                strings.insert("quit", "Zamknij");
                strings.insert("settings", "Ustawienia");
                strings.insert("clean_now", "Posprzątaj teraz");
                strings.insert("tooltip", "Mouzi");
                strings.insert("tooltip_one_pending", "Mouzi – {} plik czeka");
                strings.insert("tooltip_many_pending", "Mouzi – {} pliki czekają");
                strings.insert("popup_title", "Mouzi");
                strings.insert("settings_title", "Ustawienia Mouzi");
                strings.insert("dashboard", "Dashboard");
                strings.insert("dashboard_title", "Mouzi Dashboard");
                strings.insert("cleanup", "Cleanup");
                strings.insert("cleanup_title", "Mouzi Cleanup");
                strings.insert("organized", "Uporządkowano {} plik(i)");
                strings.insert("suggestions", "Sugestie");
                strings.insert("suggestions_title", "Mouzi Sugestie");
            }
            "it" => {
                strings.insert("quit", "Esci");
                strings.insert("settings", "Impostazioni");
                strings.insert("clean_now", "Pulisci ora");
                strings.insert("tooltip", "Mouzi");
                strings.insert("tooltip_one_pending", "Mouzi – {} file in attesa");
                strings.insert("tooltip_many_pending", "Mouzi – {} file in attesa");
                strings.insert("popup_title", "Mouzi");
                strings.insert("settings_title", "Impostazioni Mouzi");
                strings.insert("dashboard", "Dashboard");
                strings.insert("dashboard_title", "Mouzi Dashboard");
                strings.insert("cleanup", "Pulizia");
                strings.insert("cleanup_title", "Pulizia Mouzi");
                strings.insert("organized", "Organizzati {} file");
                strings.insert("suggestions", "Suggerimenti");
                strings.insert("suggestions_title", "Mouzi Suggerimenti");
            }
            "de" => {
                strings.insert("quit", "Beenden");
                strings.insert("settings", "Einstellungen");
                strings.insert("clean_now", "Jetzt aufräumen");
                strings.insert("tooltip", "Mouzi");
                strings.insert("tooltip_one_pending", "Mouzi – {} Datei wartend");
                strings.insert("tooltip_many_pending", "Mouzi – {} Dateien wartend");
                strings.insert("popup_title", "Mouzi");
                strings.insert("settings_title", "Mouzi Einstellungen");
                strings.insert("dashboard", "Dashboard");
                strings.insert("dashboard_title", "Mouzi Dashboard");
                strings.insert("cleanup", "Aufräumen");
                strings.insert("cleanup_title", "Mouzi Aufräumen");
                strings.insert("organized", "{} Datei(en) organisiert");
                strings.insert("suggestions", "Vorschläge");
                strings.insert("suggestions_title", "Mouzi Vorschläge");
            }
            "fr" => {
                strings.insert("quit", "Quitter");
                strings.insert("settings", "Paramètres");
                strings.insert("clean_now", "Nettoyer maintenant");
                strings.insert("tooltip", "Mouzi");
                strings.insert("tooltip_one_pending", "Mouzi – {} fichier en attente");
                strings.insert("tooltip_many_pending", "Mouzi – {} fichiers en attente");
                strings.insert("popup_title", "Mouzi");
                strings.insert("settings_title", "Paramètres Mouzi");
                strings.insert("dashboard", "Dashboard");
                strings.insert("dashboard_title", "Mouzi Dashboard");
                strings.insert("cleanup", "Nettoyage");
                strings.insert("cleanup_title", "Nettoyage Mouzi");
                strings.insert("organized", "{} fichier(s) organisé(s)");
                strings.insert("suggestions", "Suggestions");
                strings.insert("suggestions_title", "Mouzi Suggestions");
            }
            "ru" => {
                strings.insert("quit", "Выход");
                strings.insert("settings", "Настройки");
                strings.insert("clean_now", "Очистить сейчас");
                strings.insert("tooltip", "Mouzi");
                strings.insert("tooltip_one_pending", "Mouzi – {} файл ожидает");
                strings.insert("tooltip_many_pending", "Mouzi – {} файла ожидают");
                strings.insert("popup_title", "Mouzi");
                strings.insert("settings_title", "Настройки Mouzi");
                strings.insert("dashboard", "Dashboard");
                strings.insert("dashboard_title", "Mouzi Dashboard");
                strings.insert("cleanup", "Очистка");
                strings.insert("cleanup_title", "Очистка Mouzi");
                strings.insert("organized", "Организовано {} файл(ов)");
                strings.insert("suggestions", "Предложения");
                strings.insert("suggestions_title", "Mouzi Предложения");
            }
            "ja" => {
                strings.insert("quit", "終了");
                strings.insert("settings", "設定");
                strings.insert("clean_now", "今すぐ整理");
                strings.insert("tooltip", "Mouzi");
                strings.insert("tooltip_one_pending", "Mouzi – {} 個のファイルが待機中");
                strings.insert("tooltip_many_pending", "Mouzi – {} 個のファイルが待機中");
                strings.insert("popup_title", "Mouzi");
                strings.insert("settings_title", "Mouziの設定");
                strings.insert("dashboard", "Dashboard");
                strings.insert("dashboard_title", "Mouzi Dashboard");
                strings.insert("cleanup", "クリーンアップ");
                strings.insert("cleanup_title", "Mouziクリーンアップ");
                strings.insert("organized", "{}個のファイルを整理しました");
                strings.insert("suggestions", "提案");
                strings.insert("suggestions_title", "Mouzi 提案");
            }
            "vi" => {
                strings.insert("quit", "Thoát");
                strings.insert("settings", "Cài đặt");
                strings.insert("clean_now", "Dọn dẹp ngay");
                strings.insert("tooltip", "Mouzi");
                strings.insert("tooltip_one_pending", "Mouzi – {} tệp đang chờ");
                strings.insert("tooltip_many_pending", "Mouzi – {} tệp đang chờ");
                strings.insert("popup_title", "Mouzi");
                strings.insert("settings_title", "Cài đặt Mouzi");
                strings.insert("dashboard", "Dashboard");
                strings.insert("dashboard_title", "Mouzi Dashboard");
                strings.insert("cleanup", "Dọn dẹp");
                strings.insert("cleanup_title", "Mouzi Dọn dẹp");
                strings.insert("organized", "Đã sắp xếp {} tệp");
                strings.insert("suggestions", "Gợi ý");
                strings.insert("suggestions_title", "Mouzi Gợi ý");
            }
            "es" => {
                strings.insert("quit", "Salir");
                strings.insert("settings", "Configuración");
                strings.insert("clean_now", "Limpiar ahora");
                strings.insert("tooltip", "Mouzi");
                strings.insert("tooltip_one_pending", "Mouzi – {} archivo esperando");
                strings.insert("tooltip_many_pending", "Mouzi – {} archivos esperando");
                strings.insert("popup_title", "Mouzi");
                strings.insert("settings_title", "Configuración de Mouzi");
                strings.insert("dashboard", "Dashboard");
                strings.insert("dashboard_title", "Mouzi Dashboard");
                strings.insert("cleanup", "Limpiar");
                strings.insert("cleanup_title", "Limpiar Mouzi");
                strings.insert("organized", "{} archivo(s) organizado(s)");
                strings.insert("suggestions", "Sugerencias");
                strings.insert("suggestions_title", "Mouzi Sugerencias");
            }
            "uk" => {
                strings.insert("quit", "Вийти");
                strings.insert("settings", "Налаштування");
                strings.insert("clean_now", "Прибрати зараз");
                strings.insert("tooltip", "Mouzi");
                strings.insert("tooltip_one_pending", "Mouzi — очікує {} файл");
                strings.insert("tooltip_many_pending", "Mouzi — очікує файлів: {}");
                strings.insert("popup_title", "Mouzi");
                strings.insert("settings_title", "Налаштування Mouzi");
                strings.insert("dashboard", "Dashboard");
                strings.insert("dashboard_title", "Mouzi Dashboard");
                strings.insert("cleanup", "Прибирання");
                strings.insert("cleanup_title", "Прибирання Mouzi");
                strings.insert("organized", "Впорядковано файлів: {}");
                strings.insert("suggestions", "Пропозиції");
                strings.insert("suggestions_title", "Mouzi Пропозиції");
            }
            _ => {
                strings.insert("quit", "Quit");
                strings.insert("settings", "Settings");
                strings.insert("clean_now", "Clean Now");
                strings.insert("tooltip", "Mouzi");
                strings.insert("tooltip_one_pending", "Mouzi – {} file waiting");
                strings.insert("tooltip_many_pending", "Mouzi – {} files waiting");
                strings.insert("popup_title", "Mouzi");
                strings.insert("settings_title", "Mouzi Settings");
                strings.insert("dashboard", "Dashboard");
                strings.insert("dashboard_title", "Mouzi Dashboard");
                strings.insert("cleanup", "Cleanup");
                strings.insert("cleanup_title", "Mouzi Cleanup");
                strings.insert("organized", "Organized {} file(s)");
                strings.insert("suggestions", "Suggestions");
                strings.insert("suggestions_title", "Mouzi Suggestions");
            }
        }
        Self { strings }
    }

    pub fn get<'a>(&self, key: &'a str) -> &'a str {
        self.strings.get(key).copied().unwrap_or(key)
    }
}
