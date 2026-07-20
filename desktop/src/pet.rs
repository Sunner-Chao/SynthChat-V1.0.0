use tauri::{
    AppHandle, Manager, PhysicalPosition, WebviewUrl, WebviewWindow, WebviewWindowBuilder,
    window::Color,
};

pub const MAIN_WINDOW_LABEL: &str = "main";
pub const PET_WINDOW_LABEL: &str = "pet";

const PET_WINDOW_WIDTH: f64 = 390.0;
const PET_WINDOW_HEIGHT: f64 = 540.0;
const PET_WINDOW_MARGIN_X: i32 = 28;
const PET_WINDOW_MARGIN_Y: i32 = 64;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PetWindowOperation {
    Open,
    Toggle,
    StartDragging,
    SetIgnoreCursorEvents,
}

fn command_error(message: &str) -> String {
    message.to_owned()
}

fn allowed_window_label(operation: PetWindowOperation) -> &'static str {
    match operation {
        PetWindowOperation::Open | PetWindowOperation::Toggle => MAIN_WINDOW_LABEL,
        PetWindowOperation::StartDragging | PetWindowOperation::SetIgnoreCursorEvents => {
            PET_WINDOW_LABEL
        }
    }
}

fn require_window(window: &WebviewWindow, operation: PetWindowOperation) -> Result<(), String> {
    if window.label() == allowed_window_label(operation) {
        Ok(())
    } else {
        Err(command_error(
            "This desktop command is not available from this window.",
        ))
    }
}

fn position_on_current_monitor(window: &WebviewWindow) {
    let Ok(Some(monitor)) = window.current_monitor() else {
        return;
    };
    let monitor_position = monitor.position();
    let monitor_size = monitor.size();
    let Ok(window_size) = window.outer_size() else {
        return;
    };
    let x = monitor_position.x
        + monitor_size
            .width
            .saturating_sub(window_size.width.saturating_add(PET_WINDOW_MARGIN_X as u32))
            as i32;
    let y = monitor_position.y
        + monitor_size.height.saturating_sub(
            window_size
                .height
                .saturating_add(PET_WINDOW_MARGIN_Y as u32),
        ) as i32;
    let _ = window.set_position(PhysicalPosition::new(x, y));
}

fn reveal_pet_window(window: &WebviewWindow) -> Result<(), String> {
    if window
        .is_minimized()
        .map_err(|_| command_error("The pet window state could not be read."))?
    {
        window
            .unminimize()
            .map_err(|_| command_error("The pet window could not be restored."))?;
    }
    window
        .show()
        .map_err(|_| command_error("The pet window could not be shown."))?;
    window
        .set_focus()
        .map_err(|_| command_error("The pet window could not receive focus."))
}

fn ensure_pet_window(app: &AppHandle) -> Result<WebviewWindow, String> {
    if let Some(window) = app.get_webview_window(PET_WINDOW_LABEL) {
        return Ok(window);
    }

    let window = WebviewWindowBuilder::new(
        app,
        PET_WINDOW_LABEL,
        WebviewUrl::App("index.html?window=pet".into()),
    )
    .title("SynthPet")
    .inner_size(PET_WINDOW_WIDTH, PET_WINDOW_HEIGHT)
    .min_inner_size(PET_WINDOW_WIDTH, PET_WINDOW_HEIGHT)
    .resizable(false)
    .decorations(false)
    .transparent(true)
    .background_color(Color(0, 0, 0, 0))
    .always_on_top(true)
    .skip_taskbar(true)
    .shadow(false)
    .visible(false)
    .build()
    .map_err(|_| command_error("The pet window could not be created."))?;

    let _ = window.set_background_color(Some(Color(0, 0, 0, 0)));
    position_on_current_monitor(&window);
    Ok(window)
}

#[tauri::command]
pub fn open_pet_window(app: AppHandle, window: WebviewWindow) -> Result<(), String> {
    require_window(&window, PetWindowOperation::Open)?;
    let pet_window = ensure_pet_window(&app)?;
    reveal_pet_window(&pet_window)
}

#[tauri::command]
pub fn toggle_pet_window(app: AppHandle, window: WebviewWindow) -> Result<bool, String> {
    require_window(&window, PetWindowOperation::Toggle)?;

    let pet_window = ensure_pet_window(&app)?;
    if pet_window
        .is_visible()
        .map_err(|_| command_error("The pet window state could not be read."))?
    {
        pet_window
            .hide()
            .map_err(|_| command_error("The pet window could not be hidden."))?;
        Ok(false)
    } else {
        reveal_pet_window(&pet_window)?;
        Ok(true)
    }
}

#[tauri::command]
pub fn pet_window_start_dragging(window: WebviewWindow) -> Result<(), String> {
    require_window(&window, PetWindowOperation::StartDragging)?;
    window
        .start_dragging()
        .map_err(|_| command_error("The pet window could not start dragging."))
}

#[tauri::command]
pub fn pet_window_set_ignore_cursor_events(
    window: WebviewWindow,
    ignore: bool,
) -> Result<(), String> {
    require_window(&window, PetWindowOperation::SetIgnoreCursorEvents)?;
    window
        .set_ignore_cursor_events(ignore)
        .map_err(|_| command_error("The pet window could not update pointer handling."))
}

#[cfg(test)]
mod tests {
    use super::{MAIN_WINDOW_LABEL, PET_WINDOW_LABEL, PetWindowOperation, allowed_window_label};

    #[test]
    fn pet_bridge_operations_are_bound_to_fixed_window_origins() {
        assert_eq!(MAIN_WINDOW_LABEL, "main");
        assert_eq!(PET_WINDOW_LABEL, "pet");
        assert_ne!(MAIN_WINDOW_LABEL, PET_WINDOW_LABEL);
        assert_eq!(
            allowed_window_label(PetWindowOperation::Open),
            MAIN_WINDOW_LABEL
        );
        assert_eq!(
            allowed_window_label(PetWindowOperation::Toggle),
            MAIN_WINDOW_LABEL
        );
        assert_eq!(
            allowed_window_label(PetWindowOperation::StartDragging),
            PET_WINDOW_LABEL
        );
        assert_eq!(
            allowed_window_label(PetWindowOperation::SetIgnoreCursorEvents),
            PET_WINDOW_LABEL
        );
    }
}
