//! The connection form's SSH tunnel section: its own enable toggle, host/
//! port/user fields, auth-method selector (agent/password/key file) with
//! its conditional secret fields, and host-key policy control. This state
//! is independent of `parsed_url`/`ConnectionUrl` -- it is never written
//! into the URL field, and is read out separately (see
//! [`ConnectionForm::ssh_state`]) by whatever persists or connects with it.

use std::path::PathBuf;

use gpui::{App, Context, Div, FocusHandle, Focusable as _, Window, div, prelude::*};
use zsql_ui::button::ButtonSwitch;

use crate::connections::{HostKeyPolicy, SshAuthKind, StoredSsh};
use crate::ui::theme;

use super::ConnectionForm;

/// How the SSH section's host-key policy control is currently set: a UI
/// projection of [`HostKeyPolicy`] that leaves out the not-yet-interactive
/// `Prompt` variant, which this form does not offer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HostKeyMode {
    AcceptNew,
    KnownHosts,
}

impl ConnectionForm {
    /// The SSH tunnel section: an enable toggle, and -- while enabled --
    /// host/port/user fields, the auth-method selector with its conditional
    /// secret fields, and the host-key policy control. Only ever rendered
    /// for a network driver -- inline as a trailing row of the single-column
    /// layout while the tunnel is off, or as the two-column layout's own
    /// right-hand column once it is on.
    pub(super) fn render_ssh_section(
        &self,
        colors: zsql_ui::theme::Colors,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let toggle_selected = if self.ssh_enabled {
            "connection-form-ssh-on"
        } else {
            "connection-form-ssh-off"
        };
        let toggle = ButtonSwitch::new()
            .selected(toggle_selected)
            .add_option(
                window,
                cx,
                "connection-form-ssh-off",
                "off",
                cx.listener(|view, _event, _window, cx| view.set_ssh_enabled(false, cx)),
            )
            .add_option(
                window,
                cx,
                "connection-form-ssh-on",
                "on",
                cx.listener(|view, _event, _window, cx| view.set_ssh_enabled(true, cx)),
            );

        let header = div()
            .flex()
            .flex_row()
            .items_center()
            .child(Self::field_label("SSH tunnel", colors))
            .child(
                div()
                    .ml_auto()
                    .track_focus(&self.ssh_enabled_focus)
                    .child(toggle),
            );

        let mut section = div()
            .flex()
            .flex_col()
            .gap(theme::CONNECTION_FORM_FIELD_GAP)
            .child(header);

        if self.ssh_enabled {
            section = section
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .gap(theme::CONNECTION_FORM_ROW_GAP)
                        .child(div().flex_1().child(Self::labeled_field(
                            "SSH host",
                            colors,
                            self.ssh_host_field.clone(),
                        )))
                        .child(div().w(theme::CONNECTION_FORM_PORT_WIDTH).child(
                            Self::labeled_field("SSH port", colors, self.ssh_port_field.clone()),
                        )),
                )
                .child(Self::labeled_field(
                    "SSH user",
                    colors,
                    self.ssh_user_field.clone(),
                ))
                .child(self.render_ssh_auth_section(colors, window, cx))
                .child(self.render_ssh_host_key_section(colors, window, cx));
        }

        section
    }

    /// The auth-method selector (Agent/Password/Key file), plus whichever
    /// secret field(s) the selected method needs -- password for
    /// [`SshAuthKind::Password`], key path and passphrase for
    /// [`SshAuthKind::Key`], nothing extra for [`SshAuthKind::Agent`].
    fn render_ssh_auth_section(
        &self,
        colors: zsql_ui::theme::Colors,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let selected = match self.ssh_auth_kind {
            SshAuthKind::Agent => "connection-form-ssh-auth-agent",
            SshAuthKind::Password => "connection-form-ssh-auth-password",
            SshAuthKind::Key => "connection-form-ssh-auth-key",
        };
        let switch = ButtonSwitch::new()
            .selected(selected)
            .add_option(
                window,
                cx,
                "connection-form-ssh-auth-agent",
                "agent",
                cx.listener(|view, _event, _window, cx| {
                    view.set_ssh_auth_kind(SshAuthKind::Agent, cx);
                }),
            )
            .add_option(
                window,
                cx,
                "connection-form-ssh-auth-password",
                "password",
                cx.listener(|view, _event, _window, cx| {
                    view.set_ssh_auth_kind(SshAuthKind::Password, cx);
                }),
            )
            .add_option(
                window,
                cx,
                "connection-form-ssh-auth-key",
                "key file",
                cx.listener(|view, _event, _window, cx| {
                    view.set_ssh_auth_kind(SshAuthKind::Key, cx);
                }),
            );

        let mut wrapper = div()
            .flex()
            .flex_col()
            .gap(theme::CONNECTION_FORM_LABEL_GAP)
            .child(Self::field_label("SSH auth", colors))
            .child(div().track_focus(&self.ssh_auth_focus).child(switch));

        wrapper = match self.ssh_auth_kind {
            SshAuthKind::Agent => wrapper,
            SshAuthKind::Password => wrapper.child(Self::labeled_field(
                "SSH password",
                colors,
                self.ssh_password_field.clone(),
            )),
            SshAuthKind::Key => wrapper
                .child(Self::labeled_field(
                    "Key path",
                    colors,
                    self.ssh_key_path_field.clone(),
                ))
                .child(Self::labeled_field(
                    "Key passphrase",
                    colors,
                    self.ssh_key_passphrase_field.clone(),
                )),
        };
        wrapper
    }

    /// The host-key policy control (accept-new/known-hosts), plus the
    /// `known_hosts` path field while `KnownHosts` is selected.
    fn render_ssh_host_key_section(
        &self,
        colors: zsql_ui::theme::Colors,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let selected = match self.ssh_host_key_mode {
            HostKeyMode::AcceptNew => "connection-form-ssh-hostkey-accept-new",
            HostKeyMode::KnownHosts => "connection-form-ssh-hostkey-known-hosts",
        };
        let switch = ButtonSwitch::new()
            .selected(selected)
            .add_option(
                window,
                cx,
                "connection-form-ssh-hostkey-accept-new",
                "accept new",
                cx.listener(|view, _event, _window, cx| {
                    view.set_ssh_host_key_mode(HostKeyMode::AcceptNew, cx);
                }),
            )
            .add_option(
                window,
                cx,
                "connection-form-ssh-hostkey-known-hosts",
                "known hosts",
                cx.listener(|view, _event, _window, cx| {
                    view.set_ssh_host_key_mode(HostKeyMode::KnownHosts, cx);
                }),
            );

        let mut wrapper = div()
            .flex()
            .flex_col()
            .gap(theme::CONNECTION_FORM_LABEL_GAP)
            .child(Self::field_label("Host key", colors))
            .child(div().track_focus(&self.ssh_host_key_focus).child(switch));

        if self.ssh_host_key_mode == HostKeyMode::KnownHosts {
            wrapper = wrapper.child(Self::labeled_field(
                "Known hosts file",
                colors,
                self.ssh_known_hosts_path_field.clone(),
            ));
        }
        wrapper
    }

    /// Append the SSH section's own focus handles onto `order`, in visual
    /// top-to-bottom order: the enable toggle always (whenever the caller
    /// has already established the driver is a network one), then -- only
    /// while enabled -- host/port/user, the auth selector, whichever secret
    /// field(s) the selected auth kind needs, the host-key control, and the
    /// `known_hosts` path field while that policy is selected.
    pub(super) fn push_ssh_focus_order(&self, order: &mut Vec<FocusHandle>, cx: &App) {
        order.push(self.ssh_enabled_focus.clone());
        if !self.ssh_enabled {
            return;
        }
        order.push(self.ssh_host_field.read(cx).focus_handle(cx));
        order.push(self.ssh_port_field.read(cx).focus_handle(cx));
        order.push(self.ssh_user_field.read(cx).focus_handle(cx));
        order.push(self.ssh_auth_focus.clone());
        match self.ssh_auth_kind {
            SshAuthKind::Agent => {}
            SshAuthKind::Password => {
                order.push(self.ssh_password_field.read(cx).focus_handle(cx));
            }
            SshAuthKind::Key => {
                order.push(self.ssh_key_path_field.read(cx).focus_handle(cx));
                order.push(self.ssh_key_passphrase_field.read(cx).focus_handle(cx));
            }
        }
        order.push(self.ssh_host_key_focus.clone());
        if self.ssh_host_key_mode == HostKeyMode::KnownHosts {
            order.push(self.ssh_known_hosts_path_field.read(cx).focus_handle(cx));
        }
    }

    /// Turn the SSH tunnel section on or off.
    pub(crate) fn set_ssh_enabled(&mut self, enabled: bool, cx: &mut Context<Self>) {
        self.ssh_enabled = enabled;
        cx.notify();
    }

    /// Switch the SSH section's selected auth method.
    pub(crate) fn set_ssh_auth_kind(&mut self, kind: SshAuthKind, cx: &mut Context<Self>) {
        self.ssh_auth_kind = kind;
        cx.notify();
    }

    /// Switch the SSH section's selected host-key policy.
    pub(crate) fn set_ssh_host_key_mode(&mut self, mode: HostKeyMode, cx: &mut Context<Self>) {
        self.ssh_host_key_mode = mode;
        cx.notify();
    }

    /// Populate the SSH section from `ssh`/`secret` (e.g. on
    /// [`ConnectionForm::begin_edit`]), or reset it to the disabled default
    /// when `ssh` is `None`.
    pub(super) fn apply_ssh_state(
        &mut self,
        ssh: Option<StoredSsh>,
        secret: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let Some(ssh) = ssh else {
            self.reset_ssh_state(cx);
            return;
        };
        self.ssh_enabled = ssh.enabled;
        self.ssh_host_field
            .update(cx, |field, _cx| field.set_value_quiet(ssh.host));
        self.ssh_port_field
            .update(cx, |field, _cx| field.set_value_quiet(ssh.port.to_string()));
        self.ssh_user_field
            .update(cx, |field, _cx| field.set_value_quiet(ssh.user));
        self.ssh_auth_kind = ssh.auth_kind;

        let key_path_text = ssh
            .key_path
            .as_ref()
            .map(|path| path.display().to_string())
            .unwrap_or_default();
        self.ssh_key_path_field
            .update(cx, |field, _cx| field.set_value_quiet(key_path_text));

        let password_text = if matches!(ssh.auth_kind, SshAuthKind::Password) {
            secret.clone().unwrap_or_default()
        } else {
            String::new()
        };
        self.ssh_password_field
            .update(cx, |field, _cx| field.set_value_quiet(password_text));

        let passphrase_text = if matches!(ssh.auth_kind, SshAuthKind::Key) {
            secret.unwrap_or_default()
        } else {
            String::new()
        };
        self.ssh_key_passphrase_field
            .update(cx, |field, _cx| field.set_value_quiet(passphrase_text));

        match ssh.host_key_policy {
            HostKeyPolicy::KnownHosts(path) => {
                self.ssh_host_key_mode = HostKeyMode::KnownHosts;
                self.ssh_known_hosts_path_field.update(cx, |field, _cx| {
                    field.set_value_quiet(path.display().to_string());
                });
            }
            // `Prompt` is reserved for a not-yet-built interactive
            // confirmation this form does not offer; a connection stored
            // with it displays as accept-new rather than losing its place
            // in the layout.
            HostKeyPolicy::AcceptNew | HostKeyPolicy::Prompt => {
                self.ssh_host_key_mode = HostKeyMode::AcceptNew;
                self.ssh_known_hosts_path_field
                    .update(cx, |field, _cx| field.set_value_quiet(""));
            }
        }
        cx.notify();
    }

    /// Reset the SSH section to its disabled, empty default.
    pub(super) fn reset_ssh_state(&mut self, cx: &mut Context<Self>) {
        self.ssh_enabled = false;
        self.ssh_host_field
            .update(cx, |field, _cx| field.set_value_quiet(""));
        self.ssh_port_field
            .update(cx, |field, _cx| field.set_value_quiet(""));
        self.ssh_user_field
            .update(cx, |field, _cx| field.set_value_quiet(""));
        self.ssh_auth_kind = SshAuthKind::Agent;
        self.ssh_password_field
            .update(cx, |field, _cx| field.set_value_quiet(""));
        self.ssh_key_path_field
            .update(cx, |field, _cx| field.set_value_quiet(""));
        self.ssh_key_passphrase_field
            .update(cx, |field, _cx| field.set_value_quiet(""));
        self.ssh_host_key_mode = HostKeyMode::AcceptNew;
        self.ssh_known_hosts_path_field
            .update(cx, |field, _cx| field.set_value_quiet(""));
        cx.notify();
    }

    /// The SSH tunnel this form's own SSH section currently describes, as
    /// `(non-secret settings, secret)` -- `None` while the enable toggle is
    /// off, or while the URL's current driver is not a network one (the
    /// section is unmounted then, with no UI to clear a stale toggle). Read
    /// by [`super::super::ConnectionManagerView`] before it persists or
    /// connects with the form's Add/Edit/Connect/Test actions, since this
    /// state never rides `url_field`/[`zsql_core::ConnectionUrl`].
    #[must_use]
    pub fn ssh_state(&self, cx: &App) -> (Option<StoredSsh>, Option<String>) {
        let is_network_driver = self
            .pending_driver_id()
            .is_ok_and(crate::drivers::is_network);
        if !self.ssh_enabled || !is_network_driver {
            return (None, None);
        }

        let host = self.ssh_host_field.read(cx).value().trim().to_string();
        let port = self
            .ssh_port_field
            .read(cx)
            .value()
            .trim()
            .parse::<u16>()
            .unwrap_or(crate::config::DEFAULT_SSH_TUNNEL_PORT);
        let user = self.ssh_user_field.read(cx).value().trim().to_string();

        let key_path_text = self.ssh_key_path_field.read(cx).value().trim().to_string();
        let key_path = (!key_path_text.is_empty()).then(|| PathBuf::from(key_path_text));

        let host_key_policy = match self.ssh_host_key_mode {
            HostKeyMode::AcceptNew => HostKeyPolicy::AcceptNew,
            HostKeyMode::KnownHosts => {
                let path_text = self
                    .ssh_known_hosts_path_field
                    .read(cx)
                    .value()
                    .trim()
                    .to_string();
                HostKeyPolicy::KnownHosts(PathBuf::from(path_text))
            }
        };

        let secret = match self.ssh_auth_kind {
            SshAuthKind::Agent => None,
            SshAuthKind::Password => Some(self.ssh_password_field.read(cx).value().to_string()),
            SshAuthKind::Key => {
                let passphrase = self.ssh_key_passphrase_field.read(cx).value().to_string();
                (!passphrase.is_empty()).then_some(passphrase)
            }
        };

        let stored = StoredSsh {
            enabled: true,
            host,
            port,
            user,
            auth_kind: self.ssh_auth_kind,
            key_path,
            host_key_policy,
        };
        (Some(stored), secret)
    }
}
