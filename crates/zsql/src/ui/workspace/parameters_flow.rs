//! "Run with parameters" modal orchestration: opening it from a tab's
//! intercepted run (loading that script's remembered values), and routing
//! its confirm/cancel back into the tab model, results pane, and status
//! bar.

use std::collections::HashMap;

use gpui::{AppContext as _, Context, Entity};
use zsql_core::sql::params::Parameter;

use super::WorkspaceView;
use crate::session_store;
use crate::ui::parameters_modal::{ParametersModalEvent, ParametersModalView};
use crate::ui::tabs::{ParametersRequested, TabModel};

impl WorkspaceView {
    /// Wire `tabs`' [`ParametersRequested`] event and `parameters_modal`'s
    /// own confirm/cancel events together.
    pub(super) fn subscribe_to_parameters_events(
        tabs: &Entity<TabModel>,
        parameters_modal: &Entity<ParametersModalView>,
        cx: &mut Context<Self>,
    ) {
        cx.subscribe(tabs, |this, _tabs, evt: &ParametersRequested, cx| {
            this.handle_parameters_requested(evt, cx);
        })
        .detach();
        cx.subscribe(
            parameters_modal,
            |this, _modal, evt: &ParametersModalEvent, cx| {
                this.handle_parameters_modal_event(evt, cx);
            },
        )
        .detach();
    }

    /// A Script tab's run was intercepted because its SQL has detected
    /// parameters: load that script's remembered values and open the
    /// modal, and show the waiting state in the results pane and status
    /// bar until it closes.
    #[tracing::instrument(name = "workspace_handle_parameters_requested", skip(self, evt, cx))]
    fn handle_parameters_requested(&mut self, evt: &ParametersRequested, cx: &mut Context<Self>) {
        let history = self.load_param_history(&evt.history_key, &evt.parameters);
        self.parameters_modal.update(cx, |modal, cx| {
            modal.open(
                evt.tab_id,
                evt.script_label.clone(),
                evt.sql.clone(),
                evt.parameters.clone(),
                history,
                evt.history_key.clone(),
                evt.driver_id,
                cx,
            );
        });
        let count = self.parameters_modal.read(cx).parameter_count();
        self.results.update(cx, |results, cx| {
            results.show_waiting_for_params(count, cx);
        });
        self.footer.update(cx, |footer, cx| {
            footer.set_waiting_params_count(Some(count), cx);
        });
        cx.notify();
    }

    /// Each of `parameters`' remembered values for `history_key`, most
    /// recent first, from the active connection's persisted parameter
    /// history. Empty (no remembered values for anything) when no session
    /// directory is resolved or nothing has been persisted yet.
    fn load_param_history(
        &mut self,
        history_key: &str,
        parameters: &[Parameter],
    ) -> HashMap<String, Vec<String>> {
        let Some(session_dir) = self.session_store.active_session_dir() else {
            return HashMap::new();
        };
        let path = session_dir.join(session_store::PARAM_HISTORY_FILE_NAME);
        let file = self.session_store.param_history(&path);
        parameters
            .iter()
            .map(|parameter| {
                let key = parameter.storage_key();
                let values = file.history_for(history_key, &key).to_vec();
                (key, values)
            })
            .collect()
    }

    /// The modal confirmed (run with the filled-in values) or was
    /// cancelled: either way, clear the waiting state; a confirm also
    /// dispatches the substituted run and remembers the values entered.
    fn handle_parameters_modal_event(
        &mut self,
        evt: &ParametersModalEvent,
        cx: &mut Context<Self>,
    ) {
        self.results.update(cx, |results, cx| {
            results.clear_waiting_for_params(cx);
        });
        self.footer.update(cx, |footer, cx| {
            footer.set_waiting_params_count(None, cx);
        });

        match evt {
            ParametersModalEvent::Confirmed {
                tab_id,
                substituted_sql,
                history_key,
                values,
            } => {
                self.tabs.update(cx, |tabs, cx| {
                    tabs.run_confirmed_params(*tab_id, substituted_sql.clone(), cx);
                });
                self.persist_param_history(history_key, values, cx);
            }
            ParametersModalEvent::Cancelled => {
                self.tabs.update(cx, TabModel::resync_results);
            }
        }
        cx.notify();
    }

    /// Record `values` as `history_key`'s most recent run in the session
    /// store's cache, then spawn its write to disk under a freshly minted
    /// claim, mirroring `Self::dispatch_tab_session_save`'s fire-and-forget
    /// pattern: two rapid confirms can never let an older write clobber a
    /// newer one. A no-op when no session directory is resolved.
    #[tracing::instrument(name = "workspace_persist_param_history", skip(self, values, cx))]
    fn persist_param_history(
        &mut self,
        history_key: &str,
        values: &HashMap<String, String>,
        cx: &mut Context<Self>,
    ) {
        let Some(session_dir) = self.session_store.active_session_dir() else {
            return;
        };
        let path = session_dir.join(session_store::PARAM_HISTORY_FILE_NAME);
        let max_history = self.param_history_max;
        let (file, claim) =
            self.session_store
                .record_param_run(&path, history_key, values, max_history);
        cx.background_spawn(async move {
            if let Err(err) = claim.write_if_current(|_guard| file.save(&path)) {
                tracing::warn!(error = %err, "failed to save remembered parameter values");
            }
        })
        .detach();
    }
}
