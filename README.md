# hypr-dictate

Dictado ligero para Hyprland: graba, envía una petición a CLIProxyAPI y escribe la transcripción completa en la ventana enfocada al comenzar. Rust y Wayland nativo, sin navegador, historial ni portapapeles.

## Compilar y ejecutar

Requiere Rust para compilar; en ejecución, Hyprland con layer-shell y teclado virtual, PipeWire, `pw-record` y `hyprctl`.

```bash
cargo build --release
# CLIPROXYAPI_API_KEY debe estar ya exportada en esta terminal.
./target/release/hypr-dictate run
```

El caparazón oscuro aparece muy bajo y pegado al borde inferior. Al grabar se expande con una transición y muestra el nivel real de voz; al terminar de transcribir vuelve suavemente a su tamaño compacto. Clic izquierdo para grabar o detener; arrastre izquierdo para moverlo horizontalmente. Clic derecho abre o cierra un modal centrado de configuración. En reposo no captura audio ni mantiene animaciones activas.

En el modal centrado, cada campo (cuenta/ruta, modelo y atajo) funciona como un selector desplegable: haz clic para abrir la lista y elegir una opción, o usa la rueda para recorrerla. «Automática» muestra modelos sin prefijo. Son rutas del catálogo, no credenciales verificadas; no todos los modelos aceptan audio. Pulsa **Guardar cambios** para aplicarlos. Las preferencias sin secretos se guardan en `~/.config/hypr-dictate/settings.json`.

Endpoint: `http://192.168.0.100:8317/v1/chat/completions`. Modelo: `cic/gemini-3.5-flash-lite`. La clave se lee de `CLIPROXYAPI_API_KEY`, nunca de un archivo de configuración.

## Atajo de teclado e inicio de dictado

Con el proceso principal ejecutándose:

```bash
./target/release/hypr-dictate start
./target/release/hypr-dictate stop
# Alternativa: toggle (exclusivo para iniciar/detener dictado)
```

El atajo predeterminado es `SUPER + ALT + V` (con inicio por pulsación simple, sin necesidad de mantener presionada la tecla).

- **Iniciar dictado:** Presiona `SUPER + ALT + V` desde cualquier ventana o campo enfocado en cualquier monitor. Si el dock estaba oculto, se mostrará temporalmente mientras dure el dictado.
- **Detener e insertar:** Presiona `Enter` (o `Enter` del teclado numérico) para detener la grabación, transcribir e insertar el texto en la ventana receptora. El sistema captura el teclado exclusivamente durante la grabación y espera la liberación física de la tecla `Enter` antes de liberar la captura y proceder a la inserción, garantizando que la tecla `Enter` no se filtre ni ejecute/envíe el formulario o prompt.
- **Cancelar:** Presiona `Escape` para cancelar y descartar el audio inmediatamente sin realizar peticiones a la API, restaurando el foco en la ventana original. Al terminar o cancelar, si el dock estaba previamente oculto, se restaura a su estado oculto automáticamente.

El panel permite elegir entre combinaciones predefinidas (`SUPER + ALT + V`, `CTRL + SHIFT + D`, `CTRL + ALT + D`, `SUPER + SHIFT + D`, `SUPER + F9`, `SUPER + F10`) o desactivar el atajo.

Al guardar un cambio de atajo, comprueba conflictos en los bindings activos y modifica únicamente el bloque `-- BEGIN hypr-dictate` / `-- END hypr-dictate` de `~/.config/hypr/bindings.lua`. Valida sintaxis con `luac -p`, recarga Hyprland y restaura el archivo si detecta errores de configuración. Requiere que ese archivo siga siendo el módulo cargado por tu configuración Lua.

No instala arranque automático; el proceso principal debe seguir activo y heredar la clave. Una recarga externa puede afectar los bindings como cualquier otra configuración de Hyprland.

## Mostrar u ocultar la interfaz (toggle-ui)

Para alternar la visibilidad del dock sin alterar el estado del dictado:

```bash
./target/release/hypr-dictate toggle-ui
```

- Si el dock no está en ejecución, lo inicia en segundo plano (hereda `CLIPROXYAPI_API_KEY` del entorno de la terminal que lo invoca, queda desvinculado de la terminal y verifica que el inicio sea exitoso sin descartar errores) dejándolo visible.
- Si ya se está ejecutando y la interfaz está visible, la oculta cerrando el dock y el modal de ajustes si estaba abierto. En estado oculto no consume ciclos de dibujo Wayland ni remapeos ociosos, pero continúa atendiendo órdenes del socket y atajos de dictado.
- Si ya se está ejecutando y está oculto, vuelve a mostrar el dock respetando el ciclo configure/commit de layer-shell, preservando su monitor y desplazamiento horizontal.
- No requiere instalación global del binario: se invoca mediante la ruta en el repositorio (`./target/release/hypr-dictate toggle-ui`).
- El comando `toggle` se mantiene intacto para alternar el dictado por voz.

## Comportamiento y límites

- Captura PCM mono a 16 kHz en memoria, hasta cinco minutos (9,6 MB de PCM). Al alcanzar ese límite, la captura termina; debes detener el dictado para enviar lo capturado. WAV y base64 generan copias temporales adicionales en memoria.
- Al parar, envía una petición sin reintentos automáticos. Timeout de 120 segundos. La espera depende del servidor y la duración del audio; las pruebas previas de fragmentos cortos rondaron tres segundos, no tiempo real.
- Antes de insertar, comprueba que la ventana original sigue existiendo, la enfoca y verifica el foco. Si ya no existe, no escribe en otra ventana.
- Conserva la ventana, no la posición interna del cursor. No cambies de campo durante la petición. Las comprobaciones de foco reducen el riesgo, pero no son atómicas con la escritura: evita cambiar de ventana durante la inserción.
- **Garantía de inserción en una sola línea (no-submit):** Para evitar envíos o ejecuciones accidentales en terminales, chats o formularios web, el texto dictado convierte de forma segura todos los saltos de línea (`\r`, `\n`, `\r\n` y separadores de línea/párrafo Unicode) en espacios antes de la inserción. Esto garantiza que el teclado virtual nunca sintetice una tecla `Enter` al escribir la transcripción, impidiendo el envío no solicitado en cualquier receptor.
- Una sola grabación/petición a la vez. Hasta 240 caracteres Unicode distintos por transcripción. Respuestas truncadas o inválidas no se insertan.
- No guarda audio ni texto. Un fallo de API o de inserción descarta el resultado; una interrupción durante la escritura puede dejar texto parcial, sin reintento para evitar duplicados.
- La API usa HTTP en la LAN: clave y audio no están cifrados en tránsito. Usa solo una red de confianza.
- Usa un socket local con permisos 0600 en `$XDG_RUNTIME_DIR/hypr-dictate.sock`. Si el proceso termina abruptamente puede quedar un socket residual. Elimínalo manualmente únicamente después de comprobar que no queda otra instancia.
- El dibujo usa un archivo temporal anónimo de memoria compartida para píxeles; no contiene audio, clave ni transcripciones. El keymap temporal contiene los caracteres únicos necesarios para escribir, no su orden. El sistema puede usar swap: no se garantiza borrado seguro de memoria.

## Validar

```bash
cargo test
cargo clippy --all-targets -- -D warnings
cargo build --release
```

Prueba manual pendiente en la sesión gráfica:

1. Abre un editor vacío, sitúa el cursor y activa el botón.
2. Dicta una frase en español con tildes y ñ; detén la grabación.
3. Cambia a otra ventana mientras procesa: debe volver al editor e insertar allí.
4. Comprueba el atajo de pulsar/soltar y que no roba el foco al empezar.
5. Cierra la ventana original durante otra petición: debe mostrar error y no escribir en la ventana alternativa.

Las pruebas automáticas verifican estados, validación del destino y cabecera WAV; no sustituyen estas comprobaciones de audio, API y teclado virtual.
