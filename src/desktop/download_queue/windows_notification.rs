use anyhow::{Context, Result};
use windows::UI::Notifications::{ToastNotification, ToastNotificationManager, ToastTemplateType};
use windows::Win32::System::Registry::{HKEY_CURRENT_USER, REG_SZ, RegSetKeyValueW};
use windows::Win32::System::WinRT::{RO_INIT_MULTITHREADED, RoInitialize, RoUninitialize};
use windows::core::{HSTRING, w};

pub(super) fn notify_completed_downloads(body: &str) -> Result<()> {
    // A per-user sender identity lets the portable build send toasts without an installer.
    let name = w!("DMM");
    unsafe {
        RegSetKeyValueW(
            HKEY_CURRENT_USER,
            w!("Software\\Classes\\AppUserModelId\\com.dmm.ModManager"),
            w!("DisplayName"),
            REG_SZ.0,
            Some(name.as_ptr().cast()),
            8, // UTF-16 "DMM" and its NUL terminator.
        )
        .ok()
    }
    .context("registering DMM as a Windows notification sender")?;

    unsafe { RoInitialize(RO_INIT_MULTITHREADED) }?;
    let result = show_toast(body);
    unsafe { RoUninitialize() };
    result
}

fn show_toast(body: &str) -> Result<()> {
    let xml = ToastNotificationManager::GetTemplateContent(ToastTemplateType::ToastText02)?;
    let text = xml.GetElementsByTagName(&HSTRING::from("text"))?;
    text.Item(0)?.SetInnerText(&HSTRING::from("Download queue finished"))?;
    text.Item(1)?.SetInnerText(&HSTRING::from(body))?;
    let toast = ToastNotification::CreateToastNotification(&xml)?;
    ToastNotificationManager::CreateToastNotifierWithId(&HSTRING::from("com.dmm.ModManager"))?
        .Show(&toast)?;
    Ok(())
}
