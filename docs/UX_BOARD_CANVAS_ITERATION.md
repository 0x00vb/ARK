# Trabajo y automatizaciones: revisión de interfaz

1 de noviembre de 2026. Solicitud del propietario posterior al primer candidato.
Estado: diseño aprobado por QA independiente; implementación en verificación. La publicación anterior no contiene esta interfaz.

| Decisión | Contrato |
|---|---|
| Trabajo | Board de encargos. Reutilizar columnas y tarjetas de board.js sin su store global |
| Tarjeta | ID corto, título, responsable, origen y estado. Abre pantalla dedicada |
| Ticket | Conversación central. Propiedades laterales en ventana amplia, debajo en móvil. Aprobaciones y resultados en el mismo ticket |
| Responsable | Elegido antes de admitir el encargo. El ticket muestra el target admitido y lo conserva al continuar |
| Estado | Proyección del runtime. No permitir arrastrar una ejecución a Completed sin ejecutarla |
| Automatizaciones | Galería de diagramas y Agenda secundaria. Nueva automatización abre Canvas |
| Editor | Canvas primero. Inicio y pasos seleccionables abren inspector contextual. Pasos conserva acceso alternativo por teclado |
| Conexión | Text o Files del paso inmediatamente anterior, como exige Draft.validate. Sin ciclos ni ramas |
| Datos | Definition, Revision, Run y layout existentes. No crear una segunda copia de encargos o grafos |
| Cuenta | El proveedor y la cuenta son dueños del límite. Eliminar el modelo elegido de esa fila |

Se comparó un board independiente que copia WorkItem con un renderer compartido sobre la
proyección existente. Se elige el renderer: una ejecución conserva ID, permisos y conversación.
Se comparó un formulario separado del canvas con un inspector contextual. Se elige inspector:
selección y edición tienen la misma posición conceptual y usan el mismo borrador.

| Estado | Comportamiento comprobable |
|---|---|
| Vacío | Crear encargo abre composer General existente. Crear automatización abre diagrama editable |
| Carga | Estado visible, sin reemplazar un borrador ni el composer enfocado |
| Error o desconexión | Mensaje y reintento existentes; conservar borrador, receipt y posiciones |
| Conexión inválida | Rechazar antes de cambiar el borrador. Mantener Undo y selección |
| Ejecución pendiente | Recuperación existente; no habilitar edición que altere su snapshot |
| Móvil | Board con scroll horizontal interno. Inspector bajo canvas. Controles táctiles y zoom de página disponibles |
| Oscuro | Tokens existentes, contraste y selección visibles |
| Teclado | Tarjetas con Enter. Nodos seleccionables, puertos activables y Escape cancela conexión. Alt+flechas mueve nodos |
| Texto largo | Títulos acotados en tarjeta, texto completo en ticket; propiedades sin desbordar |

Verificación: WebKit sobre componentes reales; luego app con servidor y modelo reales,
retomar ticket, responder aprobación, conectar puertos, guardar, recargar y ejecutar la misma
secuencia. La prueba física del teléfono y la publicación permanecen pendientes.
