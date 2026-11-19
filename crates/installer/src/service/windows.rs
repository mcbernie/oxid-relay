//! Windows service control manager (SCM) integration.

use std::ffi::OsString;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::Context as _;
use windows_service::service::{
    Service, ServiceAccess, ServiceAction, ServiceActionType, ServiceErrorControl,
    ServiceFailureActions, ServiceFailureResetPeriod, ServiceInfo, ServiceStartType, ServiceState,
    ServiceType,
};
use windows_service::service_manager::{ServiceManager as Scm, ServiceManagerAccess};

use super::ServiceManager;
use crate::layout::InstallLayout;
use crate::{SERVICE_DESCRIPTION, SERVICE_NAME};

/// Name shown in `services.msc`.
pub const DISPLAY_NAME: &str = "OxidRelay";

/// How long to wait for the service to reach a requested state.
const STATE_TIMEOUT: Duration = Duration::from_secs(30);
/// Interval between status queries while waiting.
const POLL_INTERVAL: Duration = Duration::from_millis(250);

/// Delay before the SCM restarts a failed service.
const RESTART_DELAY: Duration = Duration::from_secs(5);
/// Number of restart attempts before the SCM gives up.
const RESTART_ATTEMPTS: usize = 3;
/// After this long without failures the restart counter resets.
const FAILURE_RESET: Duration = Duration::from_secs(24 * 60 * 60);

/// Win32 error codes the SCM reports that are expected, not fatal.
const ERROR_ACCESS_DENIED: i32 = 5;
const ERROR_SERVICE_NOT_ACTIVE: i32 = 1062;
const ERROR_SERVICE_DOES_NOT_EXIST: i32 = 1060;
const ERROR_SERVICE_MARKED_FOR_DELETE: i32 = 1072;

/// The Windows service manager.
#[derive(Debug, Clone, Copy, Default)]
pub struct WindowsServices;

/// Fails with a friendly message unless the process runs elevated, by
/// opening the SCM with the right to create services.
pub fn ensure_elevated() -> anyhow::Result<()> {
    connect(ServiceManagerAccess::CONNECT | ServiceManagerAccess::CREATE_SERVICE).map(drop)
}

/// Opens the local SCM, mapping "access denied" to an elevation hint.
fn connect(access: ServiceManagerAccess) -> anyhow::Result<Scm> {
    Scm::local_computer(None::<&str>, access).map_err(|err| {
        if os_code(&err) == Some(ERROR_ACCESS_DENIED) {
            anyhow::anyhow!(
                "administrator rights required: run this command from an elevated PowerShell \
                 (right-click PowerShell, \"Run as administrator\")"
            )
        } else {
            anyhow::Error::new(err).context("connecting to the service control manager")
        }
    })
}

/// The Win32 error code inside a `windows-service` error, if any.
fn os_code(err: &windows_service::Error) -> Option<i32> {
    match err {
        windows_service::Error::Winapi(io) => io.raw_os_error(),
        _ => None,
    }
}

/// Opens the relay's service, returning `None` if it is not registered.
fn open(access: ServiceAccess) -> anyhow::Result<Option<Service>> {
    let scm = connect(ServiceManagerAccess::CONNECT)?;
    match scm.open_service(SERVICE_NAME, access) {
        Ok(service) => Ok(Some(service)),
        Err(err) if os_code(&err) == Some(ERROR_SERVICE_DOES_NOT_EXIST) => Ok(None),
        Err(err) => Err(anyhow::Error::new(err).context(format!("opening service {SERVICE_NAME}"))),
    }
}

/// Opens the relay's service, failing if it is not registered.
fn open_existing(access: ServiceAccess) -> anyhow::Result<Service> {
    open(access)?.with_context(|| {
        format!("service {SERVICE_NAME} is not installed; run `oxid-relay install`")
    })
}

/// Current state of `service`.
fn state(service: &Service) -> anyhow::Result<ServiceState> {
    Ok(service
        .query_status()
        .context("querying service status")?
        .current_state)
}

/// Polls until `service` reaches `target`, failing after [`STATE_TIMEOUT`]
/// or when it stops while it should be starting.
fn wait_for(service: &Service, target: ServiceState) -> anyhow::Result<()> {
    let deadline = Instant::now() + STATE_TIMEOUT;
    loop {
        let current = state(service)?;
        if current == target {
            return Ok(());
        }
        if target == ServiceState::Running && current == ServiceState::Stopped {
            anyhow::bail!(
                "service {SERVICE_NAME} stopped right after starting; \
                 check the Application log in Event Viewer"
            );
        }
        if Instant::now() >= deadline {
            anyhow::bail!(
                "timed out after {}s waiting for service {SERVICE_NAME} to reach {target:?} \
                 (currently {current:?})",
                STATE_TIMEOUT.as_secs()
            );
        }
        thread::sleep(POLL_INTERVAL);
    }
}

/// Service configuration for `layout`: auto-start, own process, LocalSystem,
/// started as `oxid-relay.exe --service --config <config_file>`.
fn service_info(layout: &InstallLayout) -> ServiceInfo {
    ServiceInfo {
        name: OsString::from(SERVICE_NAME),
        display_name: OsString::from(DISPLAY_NAME),
        service_type: ServiceType::OWN_PROCESS,
        start_type: ServiceStartType::AutoStart,
        error_control: ServiceErrorControl::Normal,
        executable_path: layout.binary.clone(),
        launch_arguments: vec![
            OsString::from("--service"),
            OsString::from("--config"),
            layout.config_file.clone().into_os_string(),
        ],
        dependencies: Vec::new(),
        // `None` means LocalSystem.
        account_name: None,
        account_password: None,
    }
}

/// Restart after [`RESTART_DELAY`], up to [`RESTART_ATTEMPTS`] times.
fn failure_actions() -> ServiceFailureActions {
    ServiceFailureActions {
        reset_period: ServiceFailureResetPeriod::After(FAILURE_RESET),
        reboot_msg: None,
        command: None,
        actions: Some(
            (0..RESTART_ATTEMPTS)
                .map(|_| ServiceAction {
                    action_type: ServiceActionType::Restart,
                    delay: RESTART_DELAY,
                })
                .collect(),
        ),
    }
}

impl ServiceManager for WindowsServices {
    fn register(&self, layout: &InstallLayout) -> anyhow::Result<()> {
        let scm = connect(ServiceManagerAccess::CONNECT | ServiceManagerAccess::CREATE_SERVICE)?;
        let info = service_info(layout);
        let access = ServiceAccess::QUERY_STATUS | ServiceAccess::CHANGE_CONFIG;
        let service = match scm.open_service(SERVICE_NAME, access) {
            Ok(service) => {
                service
                    .change_config(&info)
                    .context("updating the existing service configuration")?;
                service
            }
            Err(err) if os_code(&err) == Some(ERROR_SERVICE_DOES_NOT_EXIST) => scm
                .create_service(&info, access)
                .context("creating the service")?,
            Err(err) => return Err(anyhow::Error::new(err).context("opening the service")),
        };
        service
            .set_description(SERVICE_DESCRIPTION)
            .context("setting the service description")?;
        service
            .update_failure_actions(failure_actions())
            .context("setting the service recovery actions")?;
        // Also restart when the service reports an error exit code, not only
        // when the process crashes.
        service
            .set_failure_actions_on_non_crash_failures(true)
            .context("enabling recovery on non-crash failures")?;
        Ok(())
    }

    fn unregister(&self) -> anyhow::Result<()> {
        self.stop()?;
        let Some(service) = open(ServiceAccess::DELETE)? else {
            return Ok(());
        };
        match service.delete() {
            Ok(()) => Ok(()),
            Err(err) if os_code(&err) == Some(ERROR_SERVICE_MARKED_FOR_DELETE) => Ok(()),
            Err(err) => Err(anyhow::Error::new(err).context("deleting the service")),
        }
    }

    fn start(&self) -> anyhow::Result<()> {
        let service = open_existing(ServiceAccess::START | ServiceAccess::QUERY_STATUS)?;
        if state(&service)? == ServiceState::Running {
            return Ok(());
        }
        service.start::<&str>(&[]).context("starting the service")?;
        wait_for(&service, ServiceState::Running)
    }

    fn stop(&self) -> anyhow::Result<()> {
        let Some(service) = open(ServiceAccess::STOP | ServiceAccess::QUERY_STATUS)? else {
            return Ok(());
        };
        if state(&service)? == ServiceState::Stopped {
            return Ok(());
        }
        match service.stop() {
            Ok(_) => {}
            Err(err) if os_code(&err) == Some(ERROR_SERVICE_NOT_ACTIVE) => return Ok(()),
            Err(err) => return Err(anyhow::Error::new(err).context("stopping the service")),
        }
        wait_for(&service, ServiceState::Stopped)
    }

    fn is_running(&self) -> anyhow::Result<bool> {
        match open(ServiceAccess::QUERY_STATUS)? {
            Some(service) => Ok(state(&service)? == ServiceState::Running),
            None => Ok(false),
        }
    }

    fn is_registered(&self) -> anyhow::Result<bool> {
        Ok(open(ServiceAccess::QUERY_STATUS)?.is_some())
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    #[test]
    fn service_info_matches_layout() {
        let layout = InstallLayout::windows(Path::new(r"C:\PF"), Path::new(r"C:\PD"));
        let info = service_info(&layout);
        assert_eq!(info.name, SERVICE_NAME);
        assert_eq!(info.executable_path, layout.binary);
        assert_eq!(
            info.launch_arguments,
            vec![
                OsString::from("--service"),
                OsString::from("--config"),
                layout.config_file.into_os_string(),
            ]
        );
        assert!(info.account_name.is_none());
    }

    #[test]
    fn failure_actions_restart_three_times() {
        let actions = failure_actions().actions.unwrap();
        assert_eq!(actions.len(), 3);
        assert!(
            actions
                .iter()
                .all(|a| a.action_type == ServiceActionType::Restart && a.delay == RESTART_DELAY)
        );
    }
}
