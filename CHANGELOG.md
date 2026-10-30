# Registro de Cambios (Changelog)

Documentación técnica de la integración de transcripción local, ciclo de vida seguro, inyección Wayland y migración de ajustes a ventana nativa GTK4 según upstream (SHA `604265374a8bca17cb0bbfc54974bb3c7976519c`).

> **Contexto de ramas y autoría:** Los commits iniciales (`39d678c`, `bcd436f` y `953be10`) fueron desarrollados originalmente en la rama local `feature/local-dictation` e integrados en `develop`. La integración actual en `feature/cute-settings-polish` incorpora la ventana nativa de preferencias GTK4 y URL configurable desde upstream preservando los avances de ciclo de vida seguro, el renderer previo del dock y el timeout estándar de 25s.

---

## Resumen de cambios y capacidades

| Módulo | Alcance técnico |
|---|---|
| `Cargo.toml` / `Cargo.lock` | Añadidas dependencias `gtk4 = "0.10"` y `gtk4-layer-shell = { version = "0.7", features = ["v1_3"] }`, conservando `xkbcommon = "0.8"`. |
| `src/preferences.rs` | Panel lateral GTK4 con `gtk4-layer-shell` (ancho 420 px, altura adaptativa, anclaje Left/Top/Bottom, namespace `hypr-dictate-preferences`, layer Top, keyboard `OnDemand`, zona exclusiva automática y scroll vertical en todas las pestañas). |
| `src/settings.rs` | Persistencia de campo `base_url` (por defecto `http://127.0.0.1:8317`), preservación del modelo predeterminado `cic/gemini-3.5-flash-lite`, atajo default `SUPER + ALT + V` y parseo de settings existentes. |
| `src/main.rs` | Validación rigurosa de URLs HTTP/HTTPS (sin credenciales, query ni fragmentos), manejo de `SaveServer`, recarga asíncrona de catálogo por generación y conservación del timeout de 25 s del cliente. |
| `src/ui.rs` | Integración del event loop GLib/GTK4 en el loop Wayland, cierre coherente de preferencias al armar dictado (`arm`), ocultar dock (`hide`) o iniciar dictado (`start`), y conservación del renderer `attach` por archivos temporales del backup previo. |
| `src/paint.rs` | Retiro del modal Cairo superpuesto y preservación exacta del caparazón del dock Wayland idéntico al backup. |
| `README.md` | Documentación del panel lateral izquierdo GTK4 con layer-shell, reserva automática de espacio hacia la derecha estilo Machete, asignación de monitor dinámico por conector y guía de configuración del servidor. |

---

## Detalle técnico por commit

### 1. `39d678c` — Endpoints locales de transcripción
- **Archivo:** `src/main.rs` (2 inserciones, 2 eliminaciones).
- **Responsabilidad:**
  - Sustituye la dirección fija de red de desarrollo (`192.168.0.100`) por el loopback local (`http://127.0.0.1:8317/v1/chat/completions` y `http://127.0.0.1:8317/v1/models?client_version=1`).
  - Mantiene el tráfico de audio y claves de API estrictamente dentro del host local por defecto.

---

### 2. `bcd436f` — Ciclo de vida seguro y planificación de texto
- **Archivo:** `src/lib.rs` (616 inserciones, 1 eliminación).
- **Responsabilidades y diseño:**
  - **IDs de generación (`generation: u64`):** Cada inicio o cancelación incrementa un contador monótono en la máquina de estados. Si una respuesta asíncrona de la API finaliza con un ID que no coincide con la generación activa, se descarta para evitar inserciones desfasadas o carreras.
  - **Espera de liberación de teclas (`KeyLifecycle` y `waiting_release`):** Al presionar `Enter` o `Escape`, la máquina de estados registra la intención y retiene la acción hasta detectar el evento físico de liberación (`KeyAction::ReleaseAndInsert` o `KeyAction::ReleaseAndCancel`). Esto previene que la pulsación física de `Enter` se filtre al campo de texto receptor al liberar la captura.
  - **Planificación de texto por bloques (`plan_chunks` y `ChunkPlan`):** Supera la limitación previa de 240 caracteres únicos. En lugar de un único keymap estático, fragmenta el texto en chunks secuenciales y asigna dinámicamente hasta 10 scancodes evdev seguros (`SAFE_EVDEV_KEYS: [2..11]`), generando un descriptor XKB mínimo por bloque.
  - **Normalización de saltos de línea (`sanitize_dictated_insertion`):** Convierte `\r`, `\n`, `\r\n` y separadores de línea o párrafo Unicode en espacios ordinarios para impedir que el teclado virtual emita códigos de tecla `Enter`.
  - **Máquina de preparación (`ArmingState`):** Modela la secuencia pura de eventos de entrada a la superficie y liberación de modificadores antes de iniciar la grabación.

---

### 3. `953be10` — Integración Wayland, visibilidad de dock y atajos
- **Archivos:** `src/main.rs`, `src/ui.rs`, `src/shortcut.rs`, `src/settings.rs`, `README.md` (914 inserciones, 230 eliminaciones).
- **Responsabilidades y diseño:**
  - **Evolución del teclado virtual Wayland:** El protocolo `zwp_virtual_keyboard_v1` ya formaba parte de la base del proyecto; este cambio implementa el transporte dinámico por bloques (`ChunkPlan`), subiendo mapas de caracteres en memoria compartida por cada chunk sin interactuar directamente con dispositivos `/dev/uinput` ni requerir permisos de hardware evdev.
  - **Captura exclusiva de teclado:** Durante la grabación, layer-shell solicita exclusividad de teclado para interceptar de manera determinista `Enter` y `Escape`, liberándola únicamente tras confirmar su soltado físico.
  - **Comando `toggle-ui` y cancelación:** Alterna la visibilidad del dock mediante socket IPC. Si se invoca durante la grabación o el procesamiento, cancela el dictado (invalida la generación para descartar la respuesta HTTP en curso y libera el teclado inmediatamente; la petición de red no se interrumpe a bajo nivel, pero su resultado es ignorado al llegar).
  - **Dock temporal:** Si el dictado se inicia con el dock oculto, este se muestra temporalmente durante la grabación y regresa a su estado oculto al completar o cancelar.
  - **Destino por ventana (`activewindow`):** Captura la dirección hexadecimal de la ventana activa vía Hyprland IPC al iniciar, verificando que permanezca activa antes de insertar texto.
  - **Atajos en Lua:** Modifica `~/.config/hypr/bindings.lua` de forma atómica dentro de un bloque delimitado, validando la sintaxis con `luac -p` antes de recargar Hyprland.

---

### 4. Integración GTK4 y URL configurable (Adopción Upstream SHA `6042653`)
- **Archivos:** `Cargo.toml`, `Cargo.lock`, `src/preferences.rs`, `src/settings.rs`, `src/main.rs`, `src/ui.rs`, `src/paint.rs`, `README.md`, `CHANGELOG.md`.
- **Responsabilidades y diseño:**
  - **Ventana nativa GTK4 (`Preferences`):** Sustituye el modal custom de Cairo por controles nativos del sistema en `dev.j0k3r.HyprDictate`.
  - **Panel lateral izquierdo Wayland (`gtk4-layer-shell`):** Configura la superficie de ajustes en el borde izquierdo de la pantalla con ancho 420 px y altura adaptativa (anchors Left, Top y Bottom), capa `Top`, namespace `hypr-dictate-preferences`, interactividad de teclado bajo demanda (`OnDemand`), zona exclusiva automática (`auto_exclusive_zone_enable`) que desplaza las ventanas en mosaico a la derecha estilo Machete, y scroll vertical independiente en las pestañas General, Servidor y Prompt e IA.
  - **Monitor dinámico por conector:** Mapea el panel al conector especificado en `settings.monitor` o delega al comportamiento nativo del compositor Wayland si está sin definir.
  - **Destrucción y liberación de reserva:** Cierre inmediato mediante Escape, botón «Cerrar», ocultación del dock (`hide` / `toggle-ui`) o inicio/armado de dictado (`arm` / `start`), destruyendo la superficie y liberando la zona exclusiva para que las ventanas retomen el ancho completo.
  - **URL configurable de servidor:** Configuración persistente de `base_url` con soporte para endpoints `/v1/chat/completions` y `/v1/models?client_version=1`, validación rigurosa de host HTTP/HTTPS sin credenciales ni query/fragmentos.
  - **Catálogo por generaciones:** La actualización del catálogo asíncrono descarta respuestas tardías de URLs anteriores si la base_url o la generación cambiaron.
  - **Preservación del renderer y timeouts:** Mantiene el renderer original con archivos temporales anónimos para el dock (`Ui::attach`) sin doble buffer `memfd` y el timeout estándar de 25s del cliente.
  - **Coexistencia con ciclo de vida seguro:** Al activar dictado o cambiar visibilidad vía `toggle-ui`, las preferencias se cierran limpiamente, impidiendo solapamientos de foco o huérfanos.

---

## Límites operativos reales

| Parámetro | Valor verificado en código | Ubicación |
|---|---|---|
| Tiempo límite de grabación | 300 s (5 minutos, 9.600.000 bytes PCM mono a 16 kHz) | `src/main.rs` (bucle de grabación y comprobación de tiempo) |
| Timeout HTTP del cliente / transcripción / catálogo | 25 s totales (5 s para conexión) | `src/main.rs` (`reqwest::blocking::ClientBuilder`) |
| Tamaño de respuesta JSON | Máximo 2.000.000 bytes | `src/main.rs` |
| Longitud de transcripción | Máximo 100.000 bytes | `src/main.rs` |
| Capacidad por keymap chunk | Hasta 10 caracteres únicos por bloque | `src/lib.rs` (`SAFE_EVDEV_KEYS`) |
