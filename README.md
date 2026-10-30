# hypr-dictate

Dictado ligero para Hyprland: graba, envía una petición a CLIProxyAPI y escribe la transcripción completa en la ventana enfocada al comenzar. Rust y Wayland nativo, sin navegador ni historial; la inserción del dictado no usa el portapapeles.

## Compilar y ejecutar

Requiere Rust, `pkg-config` y las bibliotecas de desarrollo de GTK4 para compilar. En ejecución: GTK4, un tema de iconos simbólicos (por ejemplo, Adwaita), Hyprland con layer-shell y teclado virtual, PipeWire, `pw-record` y `hyprctl`.

```bash
cargo build --release
# CLIPROXYAPI_API_KEY debe estar ya exportada en esta terminal.
./target/release/hypr-dictate run
```

El caparazón oscuro aparece muy bajo y pegado al borde inferior. Al grabar se expande con una transición y muestra el nivel real de voz; al terminar de transcribir vuelve suavemente a su tamaño compacto. Clic izquierdo para grabar o detener; arrastre izquierdo para moverlo horizontalmente. Clic derecho abre o cierra el panel lateral nativo de ajustes GTK4. Hyprland controla su posición y foco; el dock sigue siendo una superficie Wayland independiente. En reposo no captura audio ni mantiene animaciones activas.

Los ajustes se despliegan como un **panel lateral al borde izquierdo** de 420px de ancho y altura adaptativa mediante `gtk4-layer-shell`. El panel reserva automáticamente su espacio desplazando las ventanas en mosaico (*tiled*) hacia la derecha sin solapamientos, operando en capa `Top`, con namespace `hypr-dictate-preferences` e interactividad de teclado bajo demanda (`OnDemand`), permitiendo escribir en los campos de entrada sin capturar el teclado globalmente. Al cerrarse mediante Escape, el botón «Cerrar», al ocultar la interfaz (`toggle-ui`) o al comenzar a dictar, la capa se destruye de inmediato liberando la zona exclusiva reservada.

Los ajustes usan controles GTK4 nativos: desplegables con búsqueda, campo de URL y editor de prompt con selección, portapapeles, deshacer y scroll vertical en todas las pestañas. Tab permite recorrer los controles; Shift+Tab vuelve atrás. Los iconos simbólicos vienen del tema del sistema y acompañan etiquetas y ayudas, sin una biblioteca adicional de iconos.

En **General**, «Automática» muestra modelos sin prefijo. Son rutas del catálogo, no credenciales verificadas; no todos los modelos aceptan audio. Pulsa **Guardar cambios** para aplicar modelo, atajo, micrófono, pantalla y parámetros de IA. La pantalla del dock cambia al guardar, no como vista previa. Cerrar por el botón o mediante Escape descarta los cambios sin guardar y destruye la capa. Las preferencias sin secretos se guardan en `~/.config/hypr-dictate/settings.json`.

### Panel lateral y monitor en Hyprland

El panel de preferencias se gestiona a través del protocolo `wlr-layer-shell`, por lo que **no requiere reglas de ventana de Hyprland** para flotar o posicionarse:

- **Ancho y reserva:** 420 px de ancho con reserva automática de zona exclusiva (`auto_exclusive_zone_enable`), respetando barras superiores (como Waybar u omarchy bar) y márgenes de compositor sin hardcodear alturas fijas.
- **Monitor dinámico:** Si se define un conector en **Pantalla del dock** (`settings.monitor`), el panel se mapea dinámicamente al monitor GDK coincidente con dicho conector. Si se encuentra en automático («Automática (enfocada)» / cadena vacía), se delega la ubicación al compositor Wayland según su política estándar documentada, sin asumir el monitor enfocado si el compositor no lo garantiza.
- **Liberación de espacio:** El cierre con Escape, el botón «Cerrar», el inicio de dictado (`arm`/`start`) o la orden `toggle-ui` destruyen la superficie de capa inmediatamente, reintegrando el ancho reservado a las ventanas en mosaico de Hyprland.

### Servidor y modelo

La clave se lee de `CLIPROXYAPI_API_KEY`, nunca de un archivo de configuración. La URL se configura desde la interfaz:

1. Abre los ajustes con clic derecho y entra en **Servidor**.
2. Edita la URL base de CLIProxyAPI; por defecto es **`http://127.0.0.1:8317`**.
3. Pulsa **Guardar servidor**. Se guarda en `settings.json` y se recarga el catálogo sin reiniciar, aunque el servidor anterior esté caído. No aplica otros cambios pendientes de la ventana. Cerrar sin guardar descarta la edición.

Se añaden `/v1/chat/completions` para transcribir y `/v1/models?client_version=1` para el catálogo. La base admite HTTP o HTTPS, una barra final opcional y un prefijo de ruta (por ejemplo, `https://proxy.example.com/cliproxy`); no incluyas `/v1`, credenciales, parámetros ni fragmentos. Una URL vacía o inválida no se guarda. No se usa una variable de entorno para la dirección.

Los ajustes anteriores sin `base_url` usan localhost. Una URL ya guardada se conserva. No se puede cambiar el servidor mientras se graba o procesa un dictado.

Modelo predeterminado: `cic/gemini-3.5-flash-lite`. Para cambiarlo, abre los ajustes, selecciona uno del catálogo que acepte audio y pulsa **Guardar cambios**.

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
- Si ya se está ejecutando y la interfaz está visible, la oculta cerrando el dock y la ventana de ajustes si estaba abierta. Si se invoca mientras una grabación o procesamiento de audio está activo, cancela el dictado en curso (aborta la grabación o invalida el resultado de la petición vía ID de generación para descartarlo, y libera el teclado) en lugar de operar de forma transparente, y luego oculta la interfaz. En estado oculto no consume ciclos de dibujo Wayland ni remapeos ociosos, pero continúa atendiendo órdenes del socket y atajos de dictado.
- Si ya se está ejecutando y está oculto, vuelve a mostrar el dock respetando el ciclo configure/commit de layer-shell, preservando su monitor y desplazamiento horizontal.
- No requiere instalación global del binario: se invoca mediante la ruta en el repositorio (`./target/release/hypr-dictate toggle-ui`).
- El comando `toggle` se mantiene intacto para alternar el dictado por voz.

## Comportamiento y límites

- Captura PCM mono a 16 kHz en memoria, hasta cinco minutos (9,6 MB de PCM). Al alcanzar ese límite, la captura termina; debes detener el dictado para enviar lo capturado. WAV y base64 generan copias temporales adicionales en memoria.
- Al parar, envía una petición sin reintentos automáticos con timeout de 25 segundos (5 s de conexión). La espera depende del servidor y la duración del audio.
- Antes de insertar, comprueba que la ventana original sigue existiendo, la enfoca y verifica el foco. Si ya no existe, no escribe en otra ventana.
- Conserva la ventana, no la posición interna del cursor. No cambies de campo durante la petición. Las comprobaciones de foco reducen el riesgo, pero no son atómicas con la escritura: evita cambiar de ventana durante la inserción.
- **Inserción en una sola línea (prevención de saltos de línea):** Para reducir envíos o ejecuciones accidentales en terminales, chats o formularios web, el texto dictado normaliza todos los saltos de línea (`\r`, `\n`, `\r\n` y separadores de línea/párrafo Unicode) convirtiéndolos en espacios antes de la inserción. Esto asegura que el teclado virtual Wayland nunca sintetice una pulsación de tecla `Enter` al escribir la transcripción. *Nota de alcance:* la normalización previene la emisión de retornos de carro sintéticos, pero no garantiza la semántica interna de cualquier aplicación receptora (por ejemplo, clientes con atajos por longitud, patrones de caracteres o escuchas de pegado/foco).
- Una sola grabación activa. Cancelar durante «Procesando» descarta el resultado y permite comenzar otra grabación, pero la petición HTTP anterior puede seguir en curso hasta terminar o agotar su timeout.
- La inserción divide el texto en bloques dinámicos (*chunks*) con keymaps temporales de hasta 10 teclas evdev seguras, eliminando la limitación histórica de 240 caracteres únicos por transcripción sin requerir acceso directo ni permisos sobre dispositivos evdev. La respuesta JSON de la API está limitada a un máximo de 2.000.000 bytes y el texto de transcripción a un máximo de 100.000 bytes; respuestas truncadas, incompletas o excedidas no se insertan.
- No guarda audio ni texto. Un fallo de API o de inserción descarta el resultado; una interrupción durante la escritura puede dejar texto parcial, sin reintento para evitar duplicados.
- La API predeterminada apunta a localhost (`http://127.0.0.1:8317`), manteniendo el tráfico dentro del equipo vía loopback. Con una URL base HTTP (incluido el valor predeterminado), clave y audio no están cifrados en tránsito: usa solo una red de confianza. Para cifrar el transporte, configura una URL HTTPS con un certificado válido; la validación TLS permanece activa y no se siguen redirecciones.
- Usa un socket local con permisos 0600 en `$XDG_RUNTIME_DIR/hypr-dictate.sock`. Si el proceso termina abruptamente puede quedar un socket residual. Elimínalo manualmente únicamente después de comprobar que no queda otra instancia.
- El dibujo del dock usa descriptores de archivos temporales anónimos en memoria compartida para los píxeles; no contiene audio, clave ni transcripciones. GTK4 gestiona por separado el dibujo y los eventos de la ventana de ajustes de forma desacoplada del dock.
- El sistema puede usar swap: no se garantiza borrado seguro de memoria.

## Validar

```bash
cargo test
cargo clippy --all-targets -- -D warnings
cargo build --release
```

La prueba de widgets GTK está ignorada por defecto para no abrir ventanas durante `cargo test`; se ejecuta aparte con un display de pruebas aislado y el filtro `native_widgets_preserve -- --ignored --test-threads=1`.

Prueba manual pendiente en la sesión gráfica:

1. Abre un editor vacío, sitúa el cursor y activa el botón.
2. Dicta una frase en español con tildes y ñ; detén la grabación.
3. Cambia a otra ventana mientras procesa: debe volver al editor e insertar allí.
4. Comprueba el atajo de dictado (Enter para transcribir, Escape para cancelar) y que no roba el foco al empezar.
5. Cierra la ventana original durante otra petición: debe mostrar error y no escribir en la ventana alternativa.
6. En **Servidor**, edita y guarda una URL válida: debe recargar el catálogo sin reiniciar. Cierra y abre los ajustes para comprobar que se conserva. Edita otra URL y cierra sin guardar: debe conservar la anterior.
7. Con el servidor inaccesible, vuelve a **Servidor** y corrige la URL. El guardado no debe depender del catálogo anterior; una URL inválida debe mostrar un error sin aplicarse.
8. Abre y cierra los ajustes varias veces mientras grabas o procesas: el dock debe seguir animándose, sin parpadeos ni bloqueos. En el prompt, prueba flechas, selección, Ctrl+A, copiar/pegar, deshacer, rueda y barra de scroll. Comprueba también la edición de la URL y el foco con Tab.
9. Cambia la pantalla del dock y cierra sin guardar: no debe moverse. Repite y guarda: debe moverse a la pantalla elegida. Comprueba que los iconos se muestran, que la ventana se puede redimensionar y que cerrar desde Hyprland descarta el borrador.

Las pruebas automáticas verifican estados, validación del destino y cabecera WAV; no sustituyen estas comprobaciones de audio, API y teclado virtual.

## Historial de cambios

Para el detalle de implementación de los commits locales de integración, consulta [CHANGELOG.md](CHANGELOG.md).
