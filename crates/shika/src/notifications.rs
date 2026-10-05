//! Native macOS banners with a session identifier returned on activation.
//! The UI consumes `clicks` on its own executor, never from an Apple callback.
use std::path::Path;
use std::sync::mpsc::{self, Receiver};

pub fn title(project: &str, task: &str) -> String {
    format!("{project} - {task}")
}

#[cfg(target_os = "macos")]
mod native {
    use block2::{DynBlock, RcBlock};
    use objc2::{
        AnyThread, DefinedClass, define_class, msg_send, rc::Retained, runtime::ProtocolObject,
    };
    use objc2_foundation::{NSArray, NSBundle, NSError, NSObject, NSObjectProtocol, NSString};
    use objc2_user_notifications::{
        UNAuthorizationOptions, UNMutableNotificationContent, UNNotification,
        UNNotificationPresentationOptions, UNNotificationRequest, UNNotificationResponse,
        UNNotificationSettings, UNNotificationSound, UNUserNotificationCenter,
        UNUserNotificationCenterDelegate,
    };
    use std::cell::Cell;
    use std::io::Write;
    use std::path::{Path, PathBuf};
    use std::ptr::NonNull;
    use std::sync::mpsc::Sender;
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    #[derive(Clone, Default)]
    struct Diagnostics(Arc<Mutex<Option<PathBuf>>>);

    impl Diagnostics {
        fn write(&self, text: &str) {
            let guard = self.0.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(path) = &*guard
                && let Ok(mut file) = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(path)
            {
                let _ = writeln!(file, "{text}");
            }
        }
    }

    struct DelegateIvars {
        clicks: Sender<String>,
        diagnostics: Diagnostics,
    }

    define_class!(
        // NSObject allows subclassing. The ivars are thread-safe because Apple
        // invokes notification callbacks on its own queue.
        #[unsafe(super = NSObject)]
        #[ivars = DelegateIvars]
        struct ShikaNotificationDelegate;

        unsafe impl NSObjectProtocol for ShikaNotificationDelegate {}

        unsafe impl UNUserNotificationCenterDelegate for ShikaNotificationDelegate {
            #[unsafe(method(userNotificationCenter:didReceiveNotificationResponse:withCompletionHandler:))]
            fn clicked(
                &self,
                _center: &UNUserNotificationCenter,
                response: &UNNotificationResponse,
                complete: &DynBlock<dyn Fn()>,
            ) {
                let session = response.notification().request().identifier().to_string();
                self.ivars()
                    .diagnostics
                    .write(&format!("clicked={session}"));
                let _ = self.ivars().clicks.send(session);
                complete.call(());
            }

            #[unsafe(method(userNotificationCenter:willPresentNotification:withCompletionHandler:))]
            fn present(
                &self,
                _center: &UNUserNotificationCenter,
                _notification: &UNNotification,
                complete: &DynBlock<dyn Fn(UNNotificationPresentationOptions)>,
            ) {
                // Sound plays the alert attached to the content. No sound attached
                // means this option is quiet.
                complete.call((UNNotificationPresentationOptions::Banner
                    | UNNotificationPresentationOptions::List
                    | UNNotificationPresentationOptions::Sound,));
            }
        }
    );

    pub struct Native {
        center: Retained<UNUserNotificationCenter>,
        warnings: Sender<String>,
        diagnostics: Diagnostics,
        last_probe: Cell<Option<Instant>>,
        // Apple's delegate property is weak, so retain this for the app lifetime.
        _delegate: Retained<ShikaNotificationDelegate>,
    }

    impl Native {
        pub fn new(clicks: Sender<String>, warnings: Sender<String>) -> Option<Self> {
            // currentNotificationCenter raises an Objective-C exception for an
            // unbundled executable. `cargo run` stays usable without banners.
            if NSBundle::mainBundle().bundleIdentifier().as_deref()
                != Some(&*NSString::from_str("com.hieule.shika"))
            {
                let _ = warnings.send("Notifications need the bundled Shika.app. Build it with scripts/bundle-app.sh.".into());
                return None;
            }
            let diagnostics = Diagnostics::default();
            let allocated = ShikaNotificationDelegate::alloc().set_ivars(DelegateIvars {
                clicks,
                diagnostics: diagnostics.clone(),
            });
            // NSObject's init returns an initialized instance of our subclass.
            let delegate: Retained<ShikaNotificationDelegate> =
                unsafe { msg_send![super(allocated), init] };
            let center = UNUserNotificationCenter::currentNotificationCenter();
            center.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
            let authorization_warnings = warnings.clone();
            let authorization_diagnostics = diagnostics.clone();
            let authorization = RcBlock::new(
                move |allowed: objc2::runtime::Bool, error: *mut NSError| {
                    authorization_diagnostics
                        .write(&format!("authorization_granted={}", allowed.as_bool()));
                    if !error.is_null() {
                        // Apple keeps the error alive for the callback duration.
                        let _ = authorization_warnings
                            .send(format!("Notification permission failed: {}", unsafe {
                                &*error
                            }));
                    } else if !allowed.as_bool() {
                        let _ = authorization_warnings.send("Notifications are disabled. Enable Shika in System Settings > Notifications.".into());
                    }
                },
            );
            center.requestAuthorizationWithOptions_completionHandler(
                UNAuthorizationOptions::Alert | UNAuthorizationOptions::Sound,
                &authorization,
            );
            Some(Self {
                center,
                warnings,
                diagnostics,
                last_probe: Cell::new(None),
                _delegate: delegate,
            })
        }

        pub fn post(&self, session: &str, project: &str, task: &str, sound: bool) {
            self.diagnostics
                .write(&format!("posting={session} sound={sound}"));
            let content = UNMutableNotificationContent::new();
            content.setTitle(&NSString::from_str(&super::title(project, task)));
            content.setBody(&NSString::from_str("Ready to check"));
            // The system alert, the sound chosen in System Settings. A missing
            // sound is why the banner used to arrive silently.
            if sound {
                content.setSound(Some(&UNNotificationSound::defaultSound()));
            }
            let request = UNNotificationRequest::requestWithIdentifier_content_trigger(
                &NSString::from_str(session),
                &content,
                None,
            );
            let warnings = self.warnings.clone();
            let diagnostics = self.diagnostics.clone();
            let session = session.to_string();
            let completed = RcBlock::new(move |error: *mut NSError| {
                diagnostics.write(&format!("accepted={} session={session}", error.is_null()));
                if !error.is_null() {
                    let _ = warnings.send(format!("Could not post notification: {}", unsafe {
                        &*error
                    }));
                }
            });
            self.center
                .addNotificationRequest_withCompletionHandler(&request, Some(&completed));
        }

        pub fn set_diagnostics_file(&self, path: &Path) {
            let mut sidecar = path.as_os_str().to_os_string();
            sidecar.push(".notifications");
            *self.diagnostics.0.lock().unwrap_or_else(|e| e.into_inner()) = Some(sidecar.into());
            self.diagnostics
                .write("notification_center_initialized=true");
            self.refresh_diagnostics();
        }

        pub fn refresh_diagnostics(&self) {
            if self
                .diagnostics
                .0
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .is_none()
            {
                return;
            }
            if self
                .last_probe
                .get()
                .is_some_and(|last| last.elapsed() < Duration::from_secs(2))
            {
                return;
            }
            self.last_probe.set(Some(Instant::now()));
            let diagnostics = self.diagnostics.clone();
            let settings = RcBlock::new(move |settings: NonNull<UNNotificationSettings>| {
                let settings = unsafe { settings.as_ref() };
                diagnostics.write(&format!("authorization_status={} alert_setting={} notification_center_setting={} sound_setting={}",
                    settings.authorizationStatus().0, settings.alertSetting().0,
                    settings.notificationCenterSetting().0, settings.soundSetting().0));
            });
            self.center
                .getNotificationSettingsWithCompletionHandler(&settings);
            let diagnostics = self.diagnostics.clone();
            let delivered = RcBlock::new(move |notifications: NonNull<NSArray<UNNotification>>| {
                let notifications = unsafe { notifications.as_ref() };
                let identifiers = notifications
                    .iter()
                    .map(|notification| notification.request().identifier().to_string())
                    .collect::<Vec<_>>();
                diagnostics.write(&format!("delivered_identifiers={identifiers:?}"));
            });
            self.center
                .getDeliveredNotificationsWithCompletionHandler(&delivered);
        }
    }
}

pub struct Notifications {
    warnings: Receiver<String>,
    #[cfg(target_os = "macos")]
    native: Option<native::Native>,
}

impl Notifications {
    pub fn new() -> (Self, Receiver<String>) {
        let (sender, receiver) = mpsc::channel();
        let (warning_sender, warnings) = mpsc::channel();
        #[cfg(target_os = "macos")]
        let notifications = Self {
            native: native::Native::new(sender, warning_sender),
            warnings,
        };
        #[cfg(not(target_os = "macos"))]
        let notifications = {
            drop(sender);
            drop(warning_sender);
            Self { warnings }
        };
        (notifications, receiver)
    }

    /// Drain warnings from Apple's background callbacks for visible UI reporting.
    pub fn warnings(&self) -> Vec<String> {
        self.warnings.try_iter().collect()
    }

    /// Optional metadata-only diagnostics, kept beside the app's PATH report.
    pub fn set_diagnostics_file(&self, path: &Path) {
        #[cfg(target_os = "macos")]
        if let Some(native) = &self.native {
            native.set_diagnostics_file(path);
        }
        #[cfg(not(target_os = "macos"))]
        let _ = path;
    }

    /// Apple API probes are asynchronous and rate-limited to every two seconds.
    pub fn refresh_diagnostics(&self) {
        #[cfg(target_os = "macos")]
        if let Some(native) = &self.native {
            native.refresh_diagnostics();
        }
    }

    pub fn post(&self, session: &str, project: &str, task: &str, sound: bool) {
        #[cfg(target_os = "macos")]
        if let Some(native) = &self.native {
            native.post(session, project, task, sound);
        }
        #[cfg(not(target_os = "macos"))]
        let _ = (session, project, task, sound);
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn title_names_the_project_and_task() {
        assert_eq!(
            super::title("Shika", "Fix terminal"),
            "Shika - Fix terminal"
        );
    }
}
