//! The per-driver TLS-verification mode control: postgres and mysql/mariadb
//! each offer four modes (off / trust cert / verify ca / verify full);
//! mssql offers three (off / trust cert / verify full), since `tiberius`
//! has no chain-only verification mode of its own. Reads from and writes
//! back to each driver's own query-parameter encoding on the form's
//! `parsed_url`: `sslmode` for postgres, `ssl-mode` for mysql/mariadb, and
//! `encrypt`/`trustServerCertificate` for mssql.

use gpui::{Context, Div, ElementId, SharedString, Window, div, prelude::*, px, rgb};
use zsql_core::{ConnectionUrl, TlsVerify};
use zsql_mssql::params::{
    ENCRYPT_KEY as MSSQL_ENCRYPT_KEY, TRUST_CERT_ALIAS_KEY as MSSQL_TRUST_CERT_ALIAS_KEY,
    TRUST_CERT_KEY as MSSQL_TRUST_CERT_KEY, is_truthy as mssql_is_truthy,
    param_ci as mssql_param_ci,
};
use zsql_ui::{button::ButtonSwitch, theme::ActiveTheme};

use crate::ui::theme;

use super::ConnectionForm;

const PG_SSLMODE_KEY: &str = "sslmode";
const PG_SSLMODE_OFF: &str = "disable";
const PG_SSLMODE_TRUST_CERT: &str = "require";
const PG_SSLMODE_VERIFY_CA: &str = "verify-ca";
const PG_SSLMODE_VERIFY_FULL: &str = "verify-full";

const MYSQL_SSLMODE_KEY: &str = "ssl-mode";
/// The mysql driver also honors this key as a fallback when `ssl-mode` is
/// absent, so the TLS control must read it too or a URL spelled with it
/// would render as `Off` while the driver actually applies its mode.
const MYSQL_SSLMODE_FALLBACK_KEY: &str = "sslmode";
const MYSQL_SSLMODE_OFF: &str = "disabled";
const MYSQL_SSLMODE_TRUST_CERT: &str = "required";
const MYSQL_SSLMODE_VERIFY_CA: &str = "verify_ca";
const MYSQL_SSLMODE_VERIFY_FULL: &str = "verify_identity";

fn is_mysql_family(driver_id: &str) -> bool {
    driver_id == "mysql" || driver_id == "mariadb"
}

/// The query-parameter key(s) `driver_id`'s TLS control reads from and
/// writes to, so the "extra query params" note never repeats them.
pub(super) fn known_query_keys(driver_id: &str) -> Vec<&'static str> {
    if driver_id == "mssql" {
        vec![
            MSSQL_ENCRYPT_KEY,
            MSSQL_TRUST_CERT_KEY,
            MSSQL_TRUST_CERT_ALIAS_KEY,
        ]
    } else if is_mysql_family(driver_id) {
        vec![MYSQL_SSLMODE_KEY, MYSQL_SSLMODE_FALLBACK_KEY]
    } else {
        vec![PG_SSLMODE_KEY]
    }
}

/// The TLS modes `driver_id`'s control offers, in display order. mssql has
/// no chain-only verification mode of its own (`tiberius` either trusts the
/// certificate outright or verifies it fully), so its control has three
/// positions rather than postgres/mysql's four.
fn modes_for(driver_id: &str) -> &'static [TlsVerify] {
    if driver_id == "mssql" {
        &[TlsVerify::Off, TlsVerify::TrustCert, TlsVerify::VerifyFull]
    } else {
        &[
            TlsVerify::Off,
            TlsVerify::TrustCert,
            TlsVerify::VerifyCa,
            TlsVerify::VerifyFull,
        ]
    }
}

/// The TLS modes currently selectable for `driver_id`, given whether the
/// form's SSH tunnel is enabled: `verify-full` drops out for postgres and
/// mysql/mariadb while a tunnel is on, since neither driver can verify a
/// tunneled connection's hostname -- mssql verifies the real hostname
/// through its own tunnel handling regardless, so it is never capped.
pub(super) fn available_modes(driver_id: &str, ssh_enabled: bool) -> Vec<TlsVerify> {
    let modes = modes_for(driver_id);
    if ssh_enabled && (is_mysql_family(driver_id) || driver_id == "postgres") {
        modes
            .iter()
            .copied()
            .filter(|mode| *mode != TlsVerify::VerifyFull)
            .collect()
    } else {
        modes.to_vec()
    }
}

/// The TLS mode the control highlights for `driver_id`: the mode stored in
/// `parsed_url`, clamped into the currently-available set so the SSH cap
/// (verify-full -> verify-ca while a tunnel is on) shows as the mode the
/// connection will actually use rather than a phantom, unrendered option.
fn effective_selected_mode(
    driver_id: &str,
    parsed_url: Option<&ConnectionUrl>,
    ssh_enabled: bool,
) -> TlsVerify {
    let stored = parsed_url.map_or(TlsVerify::Off, |parsed| read_mode(driver_id, parsed));
    if available_modes(driver_id, ssh_enabled).contains(&stored) {
        stored
    } else {
        TlsVerify::VerifyCa
    }
}

/// Whether the SSH toggle currently caps `driver_id`'s TLS control (drops
/// verify-full), i.e. whether the "capped to verify-ca" note is shown.
fn tls_is_capped(driver_id: &str, ssh_enabled: bool) -> bool {
    ssh_enabled && available_modes(driver_id, ssh_enabled).len() < modes_for(driver_id).len()
}

/// The label every driver's control shows for `mode`. The text does not
/// depend on the driver: each mode is its own [`TlsVerify`] variant.
fn mode_label(mode: TlsVerify) -> &'static str {
    match mode {
        TlsVerify::Off => "off",
        TlsVerify::TrustCert => "trust cert",
        TlsVerify::VerifyCa => "verify ca",
        TlsVerify::VerifyFull => "verify full",
    }
}

/// A stable gpui element id for `driver_id`'s `mode` option, unique across
/// every driver's control so no two controls ever share a painted id.
fn mode_element_id(driver_id: &str, mode: TlsVerify) -> ElementId {
    let family = if driver_id == "mssql" {
        "mssql"
    } else if is_mysql_family(driver_id) {
        "mysql"
    } else {
        "postgres"
    };
    let slug = match mode {
        TlsVerify::Off => "off",
        TlsVerify::TrustCert => "trust-cert",
        TlsVerify::VerifyCa => "verify-ca",
        TlsVerify::VerifyFull => "verify-full",
    };
    ElementId::from(SharedString::from(format!(
        "connection-form-tls-{family}-{slug}"
    )))
}

pub(super) fn read_mode(driver_id: &str, parsed: &ConnectionUrl) -> TlsVerify {
    if driver_id == "mssql" {
        read_mssql_mode(parsed)
    } else if is_mysql_family(driver_id) {
        read_mysql_mode(parsed)
    } else {
        read_postgres_mode(parsed)
    }
}

fn write_mode(driver_id: &str, parsed: &mut ConnectionUrl, mode: TlsVerify) {
    if driver_id == "mssql" {
        write_mssql_mode(parsed, mode);
    } else if is_mysql_family(driver_id) {
        write_mysql_mode(parsed, mode);
    } else {
        write_postgres_mode(parsed, mode);
    }
}

fn read_postgres_mode(parsed: &ConnectionUrl) -> TlsVerify {
    match parsed.query_param(PG_SSLMODE_KEY).as_deref() {
        Some(PG_SSLMODE_VERIFY_FULL) => TlsVerify::VerifyFull,
        Some(PG_SSLMODE_VERIFY_CA) => TlsVerify::VerifyCa,
        Some(PG_SSLMODE_TRUST_CERT) => TlsVerify::TrustCert,
        _ => TlsVerify::Off,
    }
}

fn write_postgres_mode(parsed: &mut ConnectionUrl, mode: TlsVerify) {
    let value = match mode {
        TlsVerify::Off => PG_SSLMODE_OFF,
        TlsVerify::TrustCert => PG_SSLMODE_TRUST_CERT,
        TlsVerify::VerifyCa => PG_SSLMODE_VERIFY_CA,
        TlsVerify::VerifyFull => PG_SSLMODE_VERIFY_FULL,
    };
    parsed.set_query_param(PG_SSLMODE_KEY, value);
}

fn read_mysql_mode(parsed: &ConnectionUrl) -> TlsVerify {
    let value = parsed
        .query_param(MYSQL_SSLMODE_KEY)
        .or_else(|| parsed.query_param(MYSQL_SSLMODE_FALLBACK_KEY));
    match value.as_deref() {
        Some(MYSQL_SSLMODE_VERIFY_FULL) => TlsVerify::VerifyFull,
        Some(MYSQL_SSLMODE_VERIFY_CA) => TlsVerify::VerifyCa,
        Some(MYSQL_SSLMODE_TRUST_CERT) => TlsVerify::TrustCert,
        _ => TlsVerify::Off,
    }
}

fn write_mysql_mode(parsed: &mut ConnectionUrl, mode: TlsVerify) {
    let value = match mode {
        TlsVerify::Off => MYSQL_SSLMODE_OFF,
        TlsVerify::TrustCert => MYSQL_SSLMODE_TRUST_CERT,
        TlsVerify::VerifyCa => MYSQL_SSLMODE_VERIFY_CA,
        TlsVerify::VerifyFull => MYSQL_SSLMODE_VERIFY_FULL,
    };
    parsed.set_query_param(MYSQL_SSLMODE_KEY, value);
    // The driver falls back to this key only when `ssl-mode` is absent; now
    // that `ssl-mode` is set, drop it so the two never disagree.
    parsed.remove_query_param(MYSQL_SSLMODE_FALLBACK_KEY);
}

/// mssql's mode, read off `parsed`'s `encrypt`/`trustServerCertificate`
/// params: `encrypt=false` always wins as [`TlsVerify::Off`] regardless of
/// `trustServerCertificate` (there is nothing left to trust once encryption
/// itself is off); otherwise `trustServerCertificate=true` reads as
/// [`TlsVerify::TrustCert`], and its absence as [`TlsVerify::VerifyFull`].
fn read_mssql_mode(parsed: &ConnectionUrl) -> TlsVerify {
    let encrypt =
        mssql_param_ci(parsed, &[MSSQL_ENCRYPT_KEY]).is_none_or(|value| mssql_is_truthy(&value));
    if !encrypt {
        return TlsVerify::Off;
    }
    let trust_server_certificate =
        mssql_param_ci(parsed, &[MSSQL_TRUST_CERT_KEY, MSSQL_TRUST_CERT_ALIAS_KEY])
            .is_some_and(|value| mssql_is_truthy(&value));
    if trust_server_certificate {
        TlsVerify::TrustCert
    } else {
        TlsVerify::VerifyFull
    }
}

fn write_mssql_mode(parsed: &mut ConnectionUrl, mode: TlsVerify) {
    match mode {
        TlsVerify::Off => {
            parsed.set_query_param(MSSQL_ENCRYPT_KEY, "false");
            parsed.remove_query_param(MSSQL_TRUST_CERT_KEY);
            parsed.remove_query_param(MSSQL_TRUST_CERT_ALIAS_KEY);
        }
        TlsVerify::TrustCert => {
            parsed.remove_query_param(MSSQL_ENCRYPT_KEY);
            parsed.remove_query_param(MSSQL_TRUST_CERT_ALIAS_KEY);
            parsed.set_query_param(MSSQL_TRUST_CERT_KEY, "true");
        }
        // mssql's control never offers verify-ca (see `modes_for`); a
        // caller reaching this arm anyway gets full verification, the same
        // as the absence of `trustServerCertificate`.
        TlsVerify::VerifyCa | TlsVerify::VerifyFull => {
            parsed.remove_query_param(MSSQL_ENCRYPT_KEY);
            parsed.remove_query_param(MSSQL_TRUST_CERT_KEY);
            parsed.remove_query_param(MSSQL_TRUST_CERT_ALIAS_KEY);
        }
    }
}

impl ConnectionForm {
    /// The TLS-verification mode control: a selector among `driver_id`'s
    /// supported modes, with `verify-full` left out (and an inline note
    /// shown) while the SSH section is enabled for the drivers capped by it.
    pub(super) fn render_tls_control(
        &self,
        driver_id: &str,
        colors: zsql_ui::theme::Colors,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let available = available_modes(driver_id, self.ssh_enabled);
        let selected_mode =
            effective_selected_mode(driver_id, self.parsed_url.as_ref(), self.ssh_enabled);

        let mut switch = ButtonSwitch::new().selected(mode_element_id(driver_id, selected_mode));
        for mode in available.iter().copied() {
            let driver_id_owned = driver_id.to_owned();
            switch = switch.add_option(
                window,
                cx,
                mode_element_id(driver_id, mode),
                mode_label(mode),
                cx.listener(move |view, _event, _window, cx| {
                    view.set_tls_mode(&driver_id_owned, mode, cx);
                }),
            );
        }

        let capped = tls_is_capped(driver_id, self.ssh_enabled);

        let mut wrapper = div()
            .flex()
            .flex_col()
            .gap(theme::CONNECTION_FORM_LABEL_GAP)
            .child(Self::field_label("TLS", colors))
            .child(div().track_focus(&self.tls_focus).child(switch));

        if capped {
            wrapper = wrapper.child(
                div()
                    .text_size(px(theme::CONNECTION_FORM_DIVIDER_TEXT_SIZE))
                    .text_color(rgb(cx.theme().colors.text_tertiary))
                    .child("verify full is capped to verify ca while the SSH tunnel is on"),
            );
        }

        wrapper
    }

    /// Apply `mode` as `driver_id`'s TLS setting on the form's `parsed_url`,
    /// then reserialize it back into the URL field. A no-op if the URL does
    /// not currently parse.
    pub(crate) fn set_tls_mode(
        &mut self,
        driver_id: &str,
        mode: TlsVerify,
        cx: &mut Context<Self>,
    ) {
        let Some(parsed) = self.parsed_url.as_mut() else {
            return;
        };
        write_mode(driver_id, parsed, mode);
        self.reserialize_url(cx);
    }

    /// The TLS modes currently selectable for `driver_id`, given the form's
    /// current SSH-enabled state. Test helper exposing [`available_modes`]
    /// (otherwise private to this module) at the form level.
    #[cfg(test)]
    pub(crate) fn tls_available_modes_for_test(&self, driver_id: &str) -> Vec<TlsVerify> {
        available_modes(driver_id, self.ssh_enabled)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use zsql_core::{ConnectionUrl, TlsVerify};

    use super::{
        MSSQL_TRUST_CERT_ALIAS_KEY, available_modes, effective_selected_mode, known_query_keys,
        mode_element_id, mode_label, read_mode, tls_is_capped, write_mode,
    };

    fn parse(url: &str) -> ConnectionUrl {
        ConnectionUrl::parse(url).expect("test URL must parse")
    }

    const ALL_MODES: [TlsVerify; 4] = [
        TlsVerify::Off,
        TlsVerify::TrustCert,
        TlsVerify::VerifyCa,
        TlsVerify::VerifyFull,
    ];

    #[test]
    fn selected_mode_reflects_the_verify_ca_cap_once_ssh_is_on() {
        // A stored verify-full connection stays selected as verify-full with no
        // tunnel, but the SSH cap must make the control reflect verify-ca (the
        // mode actually used) rather than an option it no longer renders.
        for driver in ["postgres", "mysql", "mariadb"] {
            let mut url = parse(&format!("{driver}://host/db"));
            write_mode(driver, &mut url, TlsVerify::VerifyFull);
            assert_eq!(
                effective_selected_mode(driver, Some(&url), false),
                TlsVerify::VerifyFull,
                "{driver}: verify-full stays selected with no tunnel"
            );
            assert_eq!(
                effective_selected_mode(driver, Some(&url), true),
                TlsVerify::VerifyCa,
                "{driver}: the control reflects the verify-ca cap once SSH is on"
            );
        }
    }

    #[test]
    fn mssql_selected_mode_is_never_capped_by_the_ssh_toggle() {
        let mut url = parse("mssql://host/db");
        write_mode("mssql", &mut url, TlsVerify::VerifyFull);
        assert_eq!(
            effective_selected_mode("mssql", Some(&url), true),
            TlsVerify::VerifyFull,
            "mssql keeps full verify-full over a tunnel"
        );
    }

    #[test]
    fn the_capped_note_shows_only_for_postgres_and_mysql_with_ssh_on() {
        assert!(tls_is_capped("postgres", true));
        assert!(tls_is_capped("mysql", true));
        assert!(tls_is_capped("mariadb", true));
        assert!(
            !tls_is_capped("mssql", true),
            "mssql's TLS control is never capped by the SSH toggle"
        );
        for driver in ["postgres", "mysql", "mariadb", "mssql"] {
            assert!(
                !tls_is_capped(driver, false),
                "{driver}: nothing is capped with no tunnel"
            );
        }
    }

    // -- postgres --------------------------------------------------------

    #[test]
    fn postgres_reads_off_when_sslmode_is_absent() {
        assert_eq!(
            read_mode("postgres", &parse("postgres://host/db")),
            TlsVerify::Off
        );
    }

    #[test]
    fn postgres_round_trips_every_mode_through_sslmode() {
        for mode in ALL_MODES {
            let mut url = parse("postgres://host/db");
            write_mode("postgres", &mut url, mode);
            assert_eq!(
                read_mode("postgres", &url),
                mode,
                "mode {mode:?} round-trip"
            );
        }
    }

    #[test]
    fn postgres_writes_the_expected_sslmode_values() {
        let mut url = parse("postgres://host/db");
        write_mode("postgres", &mut url, TlsVerify::VerifyFull);
        assert_eq!(url.query_param("sslmode").as_deref(), Some("verify-full"));

        write_mode("postgres", &mut url, TlsVerify::VerifyCa);
        assert_eq!(url.query_param("sslmode").as_deref(), Some("verify-ca"));

        write_mode("postgres", &mut url, TlsVerify::TrustCert);
        assert_eq!(url.query_param("sslmode").as_deref(), Some("require"));

        write_mode("postgres", &mut url, TlsVerify::Off);
        assert_eq!(url.query_param("sslmode").as_deref(), Some("disable"));
    }

    #[test]
    fn postgres_trust_cert_writes_exactly_sslmode_require_and_preserves_other_params() {
        let mut url = parse("postgres://host/db?sslmode=disable&application_name=zsql");
        write_mode("postgres", &mut url, TlsVerify::TrustCert);
        assert_eq!(url.query_param("sslmode").as_deref(), Some("require"));
        assert_eq!(
            url.query_param("application_name").as_deref(),
            Some("zsql"),
            "an unrelated query parameter must survive the TLS edit"
        );
    }

    // -- mysql / mariadb ---------------------------------------------------

    #[test]
    fn mysql_round_trips_every_mode_through_ssl_mode() {
        for mode in ALL_MODES {
            let mut url = parse("mysql://host/db");
            write_mode("mysql", &mut url, mode);
            assert_eq!(read_mode("mysql", &url), mode, "mode {mode:?} round-trip");
        }
    }

    #[test]
    fn mariadb_round_trips_every_mode_through_ssl_mode() {
        for mode in ALL_MODES {
            let mut url = parse("mariadb://host/db");
            write_mode("mariadb", &mut url, mode);
            assert_eq!(read_mode("mariadb", &url), mode, "mode {mode:?} round-trip");
        }
    }

    #[test]
    fn mariadb_uses_the_same_ssl_mode_encoding_as_mysql() {
        let mut url = parse("mariadb://host/db");
        write_mode("mariadb", &mut url, TlsVerify::VerifyFull);
        assert_eq!(
            url.query_param("ssl-mode").as_deref(),
            Some("verify_identity")
        );
        assert_eq!(read_mode("mariadb", &url), TlsVerify::VerifyFull);
    }

    #[test]
    fn mysql_reads_the_sslmode_fallback_key_when_ssl_mode_is_absent() {
        let url = parse("mysql://host/db?sslmode=verify_ca");
        assert_eq!(read_mode("mysql", &url), TlsVerify::VerifyCa);
    }

    #[test]
    fn mysql_reads_the_sslmode_required_fallback_as_the_trust_cert_mode() {
        let url = parse("mysql://host/db?sslmode=required");
        assert_eq!(read_mode("mysql", &url), TlsVerify::TrustCert);
    }

    #[test]
    fn mysql_prefers_ssl_mode_over_the_sslmode_fallback_when_both_are_present() {
        let url = parse("mysql://host/db?ssl-mode=disabled&sslmode=verify_identity");
        assert_eq!(read_mode("mysql", &url), TlsVerify::Off);
    }

    #[test]
    fn mysql_write_clears_the_sslmode_fallback_key() {
        let mut url = parse("mysql://host/db?sslmode=verify_ca");
        write_mode("mysql", &mut url, TlsVerify::VerifyFull);
        assert_eq!(url.query_param("sslmode"), None);
        assert_eq!(
            url.query_param("ssl-mode").as_deref(),
            Some("verify_identity")
        );
    }

    #[test]
    fn mysql_trust_cert_write_clears_the_sslmode_fallback_key() {
        let mut url = parse("mysql://host/db?sslmode=verify_ca");
        write_mode("mysql", &mut url, TlsVerify::TrustCert);
        assert_eq!(url.query_param("sslmode"), None);
        assert_eq!(url.query_param("ssl-mode").as_deref(), Some("required"));
    }

    #[test]
    fn mysql_writes_the_expected_ssl_mode_values() {
        let mut url = parse("mysql://host/db");
        write_mode("mysql", &mut url, TlsVerify::VerifyFull);
        assert_eq!(
            url.query_param("ssl-mode").as_deref(),
            Some("verify_identity")
        );

        write_mode("mysql", &mut url, TlsVerify::VerifyCa);
        assert_eq!(url.query_param("ssl-mode").as_deref(), Some("verify_ca"));

        write_mode("mysql", &mut url, TlsVerify::TrustCert);
        assert_eq!(url.query_param("ssl-mode").as_deref(), Some("required"));

        write_mode("mysql", &mut url, TlsVerify::Off);
        assert_eq!(url.query_param("ssl-mode").as_deref(), Some("disabled"));
    }

    #[test]
    fn mysql_trust_cert_writes_exactly_ssl_mode_required_and_preserves_other_params() {
        let mut url = parse("mysql://host/db?ssl-mode=disabled&application_name=zsql");
        write_mode("mysql", &mut url, TlsVerify::TrustCert);
        assert_eq!(url.query_param("ssl-mode").as_deref(), Some("required"));
        assert_eq!(
            url.query_param("application_name").as_deref(),
            Some("zsql"),
            "an unrelated query parameter must survive the TLS edit"
        );
    }

    // -- mssql ---------------------------------------------------------

    #[test]
    fn mssql_reads_verify_full_by_default() {
        assert_eq!(
            read_mode("mssql", &parse("mssql://host/db")),
            TlsVerify::VerifyFull,
            "encrypt defaults to true and trustServerCertificate to false"
        );
    }

    #[test]
    fn mssql_round_trips_its_three_modes() {
        for mode in [TlsVerify::Off, TlsVerify::TrustCert, TlsVerify::VerifyFull] {
            let mut url = parse("mssql://host/db");
            write_mode("mssql", &mut url, mode);
            assert_eq!(read_mode("mssql", &url), mode, "mode {mode:?} round-trip");
        }
    }

    #[test]
    fn mssql_off_sets_encrypt_false_and_clears_trust_server_certificate() {
        let mut url = parse("mssql://host/db?trustServerCertificate=true");
        write_mode("mssql", &mut url, TlsVerify::Off);
        assert_eq!(url.query_param("encrypt").as_deref(), Some("false"));
        assert_eq!(url.query_param("trustServerCertificate"), None);
    }

    #[test]
    fn mssql_trust_cert_writes_exactly_trust_server_certificate_true_and_leaves_encrypt_default() {
        let mut url = parse("mssql://host/db");
        write_mode("mssql", &mut url, TlsVerify::TrustCert);
        assert_eq!(
            url.query_param("trustServerCertificate").as_deref(),
            Some("true")
        );
        assert_eq!(
            url.query_param("encrypt"),
            None,
            "encrypt is left at its default (true) rather than written explicitly"
        );
    }

    #[test]
    fn mssql_trust_server_certificate_true_reads_as_the_trust_cert_mode_when_encrypt_is_on() {
        let url = parse("mssql://host/db?encrypt=true&trustServerCertificate=true");
        assert_eq!(read_mode("mssql", &url), TlsVerify::TrustCert);
    }

    #[test]
    fn mssql_encrypt_false_wins_over_trust_server_certificate_true() {
        let url = parse("mssql://host/db?encrypt=false&trustServerCertificate=true");
        assert_eq!(
            read_mode("mssql", &url),
            TlsVerify::Off,
            "nothing is left to trust once encryption itself is off"
        );
    }

    #[test]
    fn mssql_reads_the_yes_spelling_of_trust_server_certificate_as_the_trust_cert_mode() {
        let url = parse("mssql://host/db?trustServerCertificate=yes");
        assert_eq!(read_mode("mssql", &url), TlsVerify::TrustCert);
    }

    #[test]
    fn mssql_reads_a_differently_cased_trust_server_certificate_key_as_the_trust_cert_mode() {
        let url = parse("mssql://host/db?TrustServerCertificate=true");
        assert_eq!(read_mode("mssql", &url), TlsVerify::TrustCert);
    }

    #[test]
    fn mssql_reads_the_snake_case_trust_server_certificate_alias_as_the_trust_cert_mode() {
        let url = parse("mssql://host/db?trust_server_certificate=true");
        assert_eq!(read_mode("mssql", &url), TlsVerify::TrustCert);
    }

    #[test]
    fn mssql_reads_a_differently_cased_encrypt_key_as_off() {
        let url = parse("mssql://host/db?ENCRYPT=false");
        assert_eq!(read_mode("mssql", &url), TlsVerify::Off);
    }

    #[test]
    fn mssql_known_query_keys_include_the_trust_server_certificate_alias() {
        assert!(known_query_keys("mssql").contains(&MSSQL_TRUST_CERT_ALIAS_KEY));
    }

    // -- labels ------------------------------------------------------------

    #[test]
    fn trust_cert_mode_label_is_the_literal_string_trust_cert() {
        // The label does not depend on which driver's control renders it,
        // so this one assertion covers mssql, postgres, and mysql/mariadb
        // alike.
        assert_eq!(mode_label(TlsVerify::TrustCert), "trust cert");
    }

    #[test]
    fn trust_cert_label_is_distinct_from_the_real_verify_ca_label() {
        assert_ne!(
            mode_label(TlsVerify::TrustCert),
            mode_label(TlsVerify::VerifyCa)
        );
    }

    #[test]
    fn postgres_and_mysql_keep_the_real_verify_ca_label() {
        assert_eq!(mode_label(TlsVerify::VerifyCa), "verify ca");
    }

    // -- capping while an SSH tunnel is enabled -----------------------------

    #[test]
    fn postgres_and_mysql_drop_verify_full_while_ssh_is_enabled() {
        for driver_id in ["postgres", "mysql", "mariadb"] {
            let modes = available_modes(driver_id, true);
            assert!(
                !modes.contains(&TlsVerify::VerifyFull),
                "{driver_id} must not offer verify-full while SSH is enabled"
            );
            assert!(modes.contains(&TlsVerify::TrustCert));
            assert!(modes.contains(&TlsVerify::VerifyCa));
            assert!(modes.contains(&TlsVerify::Off));
        }
    }

    #[test]
    fn postgres_and_mysql_offer_verify_full_while_ssh_is_disabled() {
        for driver_id in ["postgres", "mysql", "mariadb"] {
            let modes = available_modes(driver_id, false);
            assert!(modes.contains(&TlsVerify::VerifyFull));
        }
    }

    #[test]
    fn mssql_is_never_capped_by_ssh() {
        assert_eq!(
            available_modes("mssql", true),
            available_modes("mssql", false),
            "mssql's own tunnel handling verifies the real hostname regardless of SSH"
        );
        assert!(available_modes("mssql", true).contains(&TlsVerify::VerifyFull));
    }

    #[test]
    fn mssql_offers_exactly_three_modes_off_trust_cert_and_verify_full() {
        for ssh_enabled in [false, true] {
            let modes = available_modes("mssql", ssh_enabled);
            assert_eq!(
                modes,
                vec![TlsVerify::Off, TlsVerify::TrustCert, TlsVerify::VerifyFull],
                "mssql's control must always render exactly these three modes, in order"
            );
        }
    }

    #[test]
    fn postgres_and_mysql_offer_exactly_four_modes_in_order_with_no_ssh() {
        for driver_id in ["postgres", "mysql", "mariadb"] {
            assert_eq!(
                available_modes(driver_id, false),
                vec![
                    TlsVerify::Off,
                    TlsVerify::TrustCert,
                    TlsVerify::VerifyCa,
                    TlsVerify::VerifyFull,
                ],
                "{driver_id}: modes must be offered off -> trust cert -> verify ca -> verify full"
            );
        }
    }

    // -- element ids ---------------------------------------------------------

    #[test]
    fn every_mode_element_id_is_unique_across_the_three_network_drivers() {
        // mariadb intentionally shares mysql's ids (see `mode_element_id`):
        // the two schemes drive the same control and are never rendered
        // side by side, so only mssql/postgres/mysql need to be distinct
        // from one another here.
        let mut ids = HashSet::new();
        for driver_id in ["mssql", "postgres", "mysql"] {
            for mode in modes_for_test(driver_id) {
                let id = mode_element_id(driver_id, mode);
                assert!(
                    ids.insert(format!("{id}")),
                    "duplicate element id {id} for driver {driver_id} mode {mode:?}"
                );
            }
        }
    }

    #[test]
    fn mariadb_and_mysql_share_the_same_mode_element_ids() {
        for mode in ALL_MODES {
            assert_eq!(
                format!("{}", mode_element_id("mariadb", mode)),
                format!("{}", mode_element_id("mysql", mode)),
                "mariadb and mysql drive the same control, so their ids must match"
            );
        }
    }

    /// mssql's three modes vs. postgres/mysql's four, mirroring
    /// [`super::modes_for`] without exposing it outside this test.
    fn modes_for_test(driver_id: &str) -> Vec<TlsVerify> {
        if driver_id == "mssql" {
            vec![TlsVerify::Off, TlsVerify::TrustCert, TlsVerify::VerifyFull]
        } else {
            ALL_MODES.to_vec()
        }
    }
}
