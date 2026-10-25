# Política de seguridad

## Versiones soportadas

Solo la última release estable de Capi recibe correcciones de seguridad.

## Reportar una vulnerabilidad

No abras un issue público. Envía un correo a **jemoldy.242@gmail.com** con:

- Versión de Capi y sistema operativo.
- Descripción del impacto y pasos para reproducirlo.
- Prueba de concepto, si existe.
- Cualquier mitigación sugerida.

Acusaremos recibo en un plazo orientativo de 72 horas y coordinaremos la
divulgación después de publicar el parche. No se aceptan exploits contra
instalaciones de terceros.

## Alcance

Entran en el alcance el motor `capi-engine`, el proxy local, los comandos Tauri
y el código de la interfaz. Quedan fuera los servicios de YouTube y las
dependencias de terceros, que deben reportarse a sus proyectos.
