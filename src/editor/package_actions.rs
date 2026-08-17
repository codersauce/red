//! Package-manager actions are cold paths outside recursive edit dispatch.

use super::*;

impl Editor {
    pub(super) async fn execute_package_action(
        &mut self,
        action: &Action,
        buffer: &mut RenderBuffer,
        runtime: &mut Runtime,
    ) -> anyhow::Result<()> {
        match action {
            Action::ListPlugins => {
                let manager = plugin::package::PluginPackageManager::new(Config::config_dir());
                let installed = match manager.list() {
                    Ok(installed) => installed,
                    Err(error) => {
                        self.set_legacy_message(Some(error.to_string()));
                        Vec::new()
                    }
                };
                let items = plugin_manager_items(
                    &installed,
                    &self.plugin_catalog,
                    &self.plugin_catalog_url,
                );
                let picker = language_pack_picker(self, items, "Loading official catalog…", true);
                self.current_dialog = Some(Box::new(picker));
                let catalog_url = plugin::catalog::catalog_url();
                let runtime = runtime.clone();
                tokio::spawn(async move {
                    let (packages, error) =
                        match plugin::catalog::PluginCatalog::fetch(&catalog_url).await {
                            Ok(catalog) => (catalog.packages, None),
                            Err(error) => (Vec::new(), Some(error.to_string())),
                        };
                    runtime.send_request(PluginRequest::Action(
                        Action::PluginManagerCatalogLoaded {
                            catalog_url,
                            packages,
                            error,
                        },
                    ));
                });
                self.render(buffer)?;
            }
            Action::PluginManagerCatalogLoaded {
                catalog_url,
                packages,
                error,
            } => {
                if error.is_none() {
                    self.plugin_catalog = packages
                        .iter()
                        .cloned()
                        .map(|package| (package.id.clone(), package))
                        .collect();
                    self.plugin_catalog_url = catalog_url.clone();
                }
                let manager = plugin::package::PluginPackageManager::new(Config::config_dir());
                let installed = manager.list().unwrap_or_default();
                let mut items = plugin_manager_items(
                    &installed,
                    &self.plugin_catalog,
                    &self.plugin_catalog_url,
                );
                if error.is_some() {
                    items.insert(
                        0,
                        plugin_manager_action_item(
                            "retry-catalog".to_string(),
                            "Retry official catalog",
                            "The last refresh failed",
                            "Retry",
                        ),
                    );
                }
                let initial_catalog_selection = if error.is_some() {
                    Some("retry-catalog".to_string())
                } else if installed.is_empty() {
                    items
                        .iter()
                        .find(|item| item.id.starts_with("catalog:"))
                        .map(|item| item.id.clone())
                } else {
                    None
                };
                if let Some(dialog) = &mut self.current_dialog {
                    dialog.update_picker(PLUGIN_MANAGER_PICKER_ID, PickerUpdate::Items(items));
                    if let Some(selection) = initial_catalog_selection {
                        dialog.update_picker(
                            PLUGIN_MANAGER_PICKER_ID,
                            PickerUpdate::Selection(selection),
                        );
                    }
                    dialog.update_picker(
                        PLUGIN_MANAGER_PICKER_ID,
                        PickerUpdate::Status(Some(error.as_ref().map_or_else(
                            || {
                                format!(
                                    "{} pack{} · Enter open",
                                    self.plugin_catalog.len(),
                                    if self.plugin_catalog.len() == 1 {
                                        ""
                                    } else {
                                        "s"
                                    }
                                )
                            },
                            |error| format!("Catalog unavailable: {error} · Enter to retry"),
                        ))),
                    );
                    dialog.update_picker(PLUGIN_MANAGER_PICKER_ID, PickerUpdate::Busy(false));
                }
                self.render(buffer)?;
            }
            Action::PluginManagerSelect(selection) => {
                if selection == "retry-catalog" {
                    self.set_legacy_message(Some(
                        "Retrying official language-pack catalog…".to_string(),
                    ));
                    runtime.send_request(PluginRequest::Action(Action::ListPlugins));
                } else if selection == "custom-source" {
                    self.current_dialog = Some(Box::new(InputPrompt::new(
                        self,
                        "GitHub owner/repo[@ref] or local path",
                        "",
                        Action::PluginManagerInstall,
                    )));
                } else if let Some(raw_id) = selection.strip_prefix("unavailable:") {
                    let id = plugin::package::PluginId::parse(raw_id)?;
                    let message = self
                        .plugin_catalog
                        .get(&id)
                        .and_then(|package| catalog_package_availability(package).1)
                        .unwrap_or_else(|| {
                            format!(
                                "Language pack `{id}` changed availability; select it again to continue."
                            )
                        });
                    self.set_legacy_message(Some(message.clone()));
                    let manager = plugin::package::PluginPackageManager::new(Config::config_dir());
                    let installed = manager.list().unwrap_or_default();
                    let items = plugin_manager_items(
                        &installed,
                        &self.plugin_catalog,
                        &self.plugin_catalog_url,
                    );
                    let picker = language_pack_picker(self, items, message, false);
                    self.current_dialog = Some(Box::new(picker));
                } else if let Some(raw_id) = selection.strip_prefix("installed:") {
                    let id = plugin::package::PluginId::parse(raw_id)?;
                    let manager = plugin::package::PluginPackageManager::new(Config::config_dir());
                    let installed = manager
                        .installed(&id)?
                        .ok_or_else(|| anyhow::anyhow!("plugin `{id}` is no longer installed"))?;
                    let toggle = if installed.enabled {
                        "disable"
                    } else {
                        "enable"
                    };
                    let mut actions = vec![plugin_manager_action_item(
                        format!("update\t{id}"),
                        "Check for updates",
                        format!("Installed v{}", installed.version),
                        "Update",
                    )];
                    actions.push(plugin_manager_action_item(
                        format!("{toggle}\t{id}"),
                        if installed.enabled {
                            "Disable language pack"
                        } else {
                            "Enable language pack"
                        },
                        if installed.enabled {
                            "Preserves files and saved data"
                        } else {
                            "Restore language support"
                        },
                        if installed.enabled {
                            "Disable"
                        } else {
                            "Enable"
                        },
                    ));
                    if installed.has_native_grammars {
                        actions.push(plugin_manager_action_item(
                            format!("trust\t{id}"),
                            "Approve native grammar",
                            "Trust the currently installed grammar bytes",
                            "Warning",
                        ));
                    }
                    actions.push(plugin_manager_action_item(
                        format!("remove\t{id}"),
                        "Remove language pack",
                        "Saved data is preserved",
                        "Delete",
                    ));
                    let status = format!(
                        "v{} · {} · {}",
                        installed.version,
                        if installed.enabled {
                            "Enabled"
                        } else {
                            "Disabled"
                        },
                        installed.languages.join(", ")
                    );
                    let picker = Picker::builder()
                        .title(&installed.name)
                        .structured_items(actions)
                        .id(PLUGIN_MANAGER_ACTION_PICKER_ID)
                        .placeholder("Filter actions")
                        .status(status)
                        .content_sized(84, 8)
                        .select_action(|operation| {
                            if let Some(package) = operation.strip_prefix("trust\t") {
                                Action::PluginManagerTrustConsent(package.to_string())
                            } else {
                                Action::PluginManagerAction(operation)
                            }
                        })
                        .build(self);
                    self.current_dialog = Some(Box::new(picker));
                } else if let Some(raw_id) = selection.strip_prefix("catalog:") {
                    let id = plugin::package::PluginId::parse(raw_id)?;
                    let manager = plugin::package::PluginPackageManager::new(Config::config_dir());
                    let replacing_custom = manager.installed(&id)?.is_some_and(|installed| {
                        !installed_from_catalog(&installed, &self.plugin_catalog_url, &id)
                    });
                    let package = self.plugin_catalog.get(&id).ok_or_else(|| {
                        anyhow::anyhow!("language pack `{id}` is no longer in the catalog")
                    })?;
                    anyhow::ensure!(
                        package.supports_current_red_release()?,
                        "language pack `{id}` requires Red API `{}`, which this Red release does not support",
                        package.red_api
                    );
                    let artifact = package
                        .artifact(crate::language::host_target())
                        .ok_or_else(|| {
                            anyhow::anyhow!(
                                "language pack `{id}` is unavailable for `{}`",
                                crate::language::host_target()
                            )
                        })?;
                    let has_native_grammars = !artifact.grammars.is_empty();
                    let mut choices = Vec::new();
                    if has_native_grammars {
                        choices.push(plugin_manager_action_item(
                            "install-and-trust".to_string(),
                            "Install with syntax highlighting",
                            format!("Approve {} verified grammar(s)", artifact.grammars.len()),
                            "Warning",
                        ));
                    }
                    choices.push(plugin_manager_action_item(
                        "install".to_string(),
                        if has_native_grammars {
                            "Install without native highlighting"
                        } else {
                            "Install language pack"
                        },
                        if has_native_grammars {
                            "Native grammar remains disabled"
                        } else {
                            "No native grammar approval required"
                        },
                        "Install",
                    ));
                    choices.push(plugin_manager_action_item(
                        "cancel".to_string(),
                        "Back to language packs",
                        "Leave the package unchanged",
                        "Cancel",
                    ));
                    let confirmed_package = package.clone();
                    let confirmed_catalog_url = self.plugin_catalog_url.clone();
                    let mut status = catalog_package_action_status(package);
                    if replacing_custom {
                        status.push_str(" · Replaces custom install");
                    }
                    let picker = Picker::builder()
                        .title(&format!("Install {}", package.name))
                        .structured_items(choices)
                        .id(PLUGIN_MANAGER_INSTALL_PICKER_ID)
                        .placeholder("Filter install options")
                        .status(status)
                        .content_sized(84, 8)
                        .select_action(move |choice| match choice.as_str() {
                            "cancel" => Action::ListPlugins,
                            "install-and-trust" => Action::PluginManagerCatalogConsent {
                                catalog_url: confirmed_catalog_url.clone(),
                                package: Box::new(confirmed_package.clone()),
                            },
                            _ => confirmed_catalog_install_action(
                                &confirmed_catalog_url,
                                &confirmed_package,
                                false,
                            ),
                        })
                        .build(self);
                    self.current_dialog = Some(Box::new(picker));
                }
                self.render(buffer)?;
            }
            Action::PluginManagerCatalogConsent {
                catalog_url,
                package,
            } => {
                let message = native_grammar_consent_message(package)?;
                let accept = confirmed_catalog_install_action(catalog_url, package, true);
                let cancel = Action::PluginManagerSelect(format!("catalog:{}", package.id));
                self.current_dialog = Some(Box::new(Confirmation::new_actions(
                    self,
                    format!("Approve native grammars for {}", package.name),
                    message,
                    "Approve and install",
                    "Back",
                    accept,
                    cancel,
                )));
                self.render(buffer)?;
            }
            Action::PluginManagerTrustConsent(raw_id) => {
                let id = plugin::package::PluginId::parse(raw_id)?;
                let manager = plugin::package::PluginPackageManager::new(Config::config_dir());
                let installed = manager
                    .installed(&id)?
                    .ok_or_else(|| anyhow::anyhow!("plugin `{id}` is no longer installed"))?;
                let digests = manager.package_grammar_digests(&id)?;
                let message = installed_grammar_consent_message(&installed, &digests)?;
                let accept = Action::PluginManagerTrustConfirmed {
                    package: id.to_string(),
                    digests,
                };
                let cancel = Action::PluginManagerSelect(format!("installed:{id}"));
                self.current_dialog = Some(Box::new(Confirmation::new_actions(
                    self,
                    format!("Approve native grammars for {}", installed.name),
                    message,
                    "Approve exact bytes",
                    "Back",
                    accept,
                    cancel,
                )));
                self.render(buffer)?;
            }
            Action::PluginManagerTrustConfirmed { package, digests } => {
                let package = package.clone();
                let digests = digests.clone();
                self.set_legacy_message(Some("Approving native grammar bytes…".to_string()));
                let runtime = runtime.clone();
                tokio::spawn(async move {
                    let manager = plugin::package::PluginPackageManager::new(Config::config_dir());
                    let result = (|| -> anyhow::Result<PluginManagerOperationOutcome> {
                        let id = plugin::package::PluginId::parse(&package)?;
                        let installed = manager
                            .installed(&id)?
                            .ok_or_else(|| anyhow::anyhow!("plugin `{id}` is not installed"))?;
                        manager.trust_package_grammars_exact(&id, &digests)?;
                        Ok(PluginManagerOperationOutcome {
                            message: format!(
                                "Approved the confirmed native grammars for {}.",
                                installed.name
                            ),
                            reload_languages: installed.has_languages,
                            restart_plugins: false,
                        })
                    })();
                    let (message, reload_languages, restart_plugins) = match result {
                        Ok(outcome) => (
                            outcome.message,
                            outcome.reload_languages,
                            outcome.restart_plugins,
                        ),
                        Err(error) => (
                            format!("Native grammar approval failed: {error}"),
                            false,
                            false,
                        ),
                    };
                    runtime.send_request(PluginRequest::Action(Action::PluginManagerFinished {
                        message,
                        reload_languages,
                        restart_plugins,
                    }));
                });
            }
            Action::PluginManagerCatalogInstall {
                catalog_url,
                package,
                trust_native_grammars,
            } => {
                let trust_native_grammars = *trust_native_grammars;
                let catalog_url = catalog_url.clone();
                let package = (**package).clone();
                self.set_legacy_message(Some(format!("Installing {}…", package.name)));
                let runtime = runtime.clone();
                tokio::spawn(async move {
                    let manager = plugin::package::PluginPackageManager::new(Config::config_dir());
                    let result = manager
                        .install_catalog_package(&catalog_url, &package, trust_native_grammars)
                        .await;
                    let (message, reload_languages) = match result {
                        Ok(package)
                            if package.has_native_grammars && !trust_native_grammars => (
                            format!(
                                "Installed {} {}. Its native grammar remains disabled until you approve it.",
                                package.name, package.version
                            ),
                            false,
                        ),
                        Ok(package) => {
                            let reload_languages = package.has_languages;
                            (
                                format!("Installed {} {}.", package.name, package.version),
                                reload_languages,
                            )
                        }
                        Err(error) => (format!("Language pack install failed: {error}"), false),
                    };
                    runtime.send_request(PluginRequest::Action(Action::PluginManagerFinished {
                        message,
                        reload_languages,
                        restart_plugins: false,
                    }));
                });
            }
            Action::PluginManagerInstall(source) => {
                let source = source.trim().to_string();
                if source.is_empty() {
                    self.set_legacy_message(Some("plugin source cannot be empty".to_string()));
                } else {
                    self.set_legacy_message(Some(format!("Installing plugin from {source}…")));
                    let runtime = runtime.clone();
                    tokio::spawn(async move {
                        let manager =
                            plugin::package::PluginPackageManager::new(Config::config_dir());
                        let result = if Path::new(&source).exists() {
                            manager.install_path(Path::new(&source)).await
                        } else {
                            let (repository, version) = source
                                .rsplit_once('@')
                                .map_or((source.as_str(), None), |(repository, version)| {
                                    (repository, Some(version))
                                });
                            manager.install_github(repository, version).await
                        };
                        let (message, reload_languages, restart_plugins) = match result {
                            Ok(plugin) => (
                                if plugin.has_native_grammars {
                                    format!(
                                        "Installed {} {}. Its native grammar remains disabled until you approve it.",
                                        plugin.name, plugin.version
                                    )
                                } else {
                                    format!("Installed {} {}.", plugin.name, plugin.version)
                                },
                                plugin.has_languages && !plugin.has_native_grammars,
                                plugin.has_husk || plugin.has_companion,
                            ),
                            Err(error) => (format!("Plugin install failed: {error}"), false, false),
                        };
                        runtime.send_request(PluginRequest::Action(
                            Action::PluginManagerFinished {
                                message,
                                reload_languages,
                                restart_plugins,
                            },
                        ));
                    });
                }
            }
            Action::PluginManagerAction(operation) => {
                let operation = operation.clone();
                self.set_legacy_message(Some("Updating plugin installation…".to_string()));
                let runtime = runtime.clone();
                tokio::spawn(async move {
                    let manager = plugin::package::PluginPackageManager::new(Config::config_dir());
                    let result = apply_plugin_manager_operation(&manager, &operation).await;
                    let (message, reload_languages, restart_plugins) = match result {
                        Ok(outcome) => (
                            outcome.message,
                            outcome.reload_languages,
                            outcome.restart_plugins,
                        ),
                        Err(error) => (format!("Plugin operation failed: {error}"), false, false),
                    };
                    runtime.send_request(PluginRequest::Action(Action::PluginManagerFinished {
                        message,
                        reload_languages,
                        restart_plugins,
                    }));
                });
            }
            Action::PluginManagerFinished {
                message,
                reload_languages,
                restart_plugins,
            } => {
                let mut message = message.clone();
                if *reload_languages {
                    match self.reload_languages().await {
                        Ok(_) => message.push_str(" Language definitions reloaded."),
                        Err(error) => message.push_str(&format!(
                            " The package was changed, but language reload failed: {error}"
                        )),
                    }
                }
                if *restart_plugins {
                    message.push_str(" Restart Red to refresh plugin code.");
                }
                self.set_legacy_message(Some(message.clone()));
                let manager = plugin::package::PluginPackageManager::new(Config::config_dir());
                let installed = manager.list().unwrap_or_default();
                let items = plugin_manager_items(
                    &installed,
                    &self.plugin_catalog,
                    &self.plugin_catalog_url,
                );
                self.current_dialog =
                    Some(Box::new(language_pack_picker(self, items, message, false)));
                self.render(buffer)?;
            }
            _ => unreachable!("non-package action routed to package dispatcher"),
        }
        Ok(())
    }
}
