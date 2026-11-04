# Chats, trabajo y automatizaciones

Propuesta para implementación con Terra. 31 de octubre de 2026.
Estado: plan, no implementación ni decisión de arquitectura aceptada.
Propietario: Codex UX research. Board: openbot-ux-plan-20261031.

## Resultado buscado

Empezar trabajo desde un objetivo, sin conocer de antemano qué agente lo hará.
Guardar ese trabajo y ejecutarlo manualmente, a una hora o al recibir un evento.
Ver qué sucede, responder preguntas y recuperar resultados en el mismo lugar.

Recomendación: dos sprints. Mantener Nuevo chat como entrada principal.
Añadir Trabajo como bandeja de actividad y Automatizaciones como biblioteca/editor.
Rutina, horario y workflow son formas del mismo trabajo reutilizable. No son tres destinos.
Los proyectos, chats, agentes, capacidades y aprobaciones de ARK conservan su identidad.

## Evidencia y límites

| Tipo | Evidencia | Qué establece |
|---|---|---|
| Observado | https://openbot.run/, navegación y captura visual del hero | Promesa breve, descarga por plataforma y app interactiva incrustada |
| Observado | https://openbot.run/app | Pide email y acepta términos al continuar. No se creó cuenta |
| Observado | https://openbot.run/app-preview, Workflows | Canvas vacío con Add trigger. Trigger cambia entre Schedule, Manual y Webhook en su propio bloque |
| Observado | Workflows, Frequency Custom | Cron de cinco campos, valor inicial `0 9 * * *`, próxima ejecución visible |
| Observado | Add node | Trigger, Prompt, LLM, Agent, Condition, Approval, Output. Agent expone tarea, agente, modelo y puertos de texto/imagen/archivo |
| Observado | Tasks | Escribir objetivo, Atlas preseleccionado, Start task. Crea fila Waiting, luego Working y Answered. Open muestra hilo propio y Open in chat |
| Observado | Respuesta de la tarea sintética | Dice literalmente `Mock task result`. Prueba interacción simulada, no ejecución real ni calidad del agente |
| Observado | More > Routines | Canvas de agentes conectados, disparadores, resultados por paso y panel con historial/próximos días |
| Observado | More > Schedule | Día/semana/mes y próximas ejecuciones, zona visible |
| Observado | Fixtures de Routines/Schedule | Etiquetas de días y horas no coinciden con algunas próximas fechas. No inferir un defecto del scheduler real a partir de la demo |
| Observado | ARK instalado, URL interna Core0.31.0/Desktop0.9.14 | New chat tiene composer y Conversation options. Schedules ofrece List/Calendar; vacío remite a pedirlo por chat |
| Fuente | Main Core ea700ed, Desktop b501263 | Son otra revisión que la app instalada. Inspección de código no equivale a probar ese código instalado |
| Inferido | Comparación anterior | La separación entre identidad del agente y conversación del encargo ayuda a iniciar trabajo sin contaminar otros encargos |
| Hipótesis | Diseño propuesto | Una biblioteca con lista/agenda/canvas reducirá decisiones y cambios de pantalla. Requiere prueba con personas |

Evidencia pública guardada en `../../board/evidence/openbot-ux-20261031/`:
`openbot-workflow-cron.{png,txt}`, `openbot-task.{png,txt}`,
`openbot-schedule.{png,txt}` y observación textual `ark-schedules.txt`.
No se generó dirección webhook, conectó cuenta, publicó flujo ni hizo llamada a modelos.
No se probó persistencia real, seguridad, sincronización o rendimiento de Openbot.

## Lo que conviene aprender y mejorar

| Referencia | Aplicación en ARK | Mejora adicional |
|---|---|---|
| Configurar el trigger dentro del nodo | Inicio inline: Manual, Horario, Al recibir un evento | Zona y próximas tres fechas calculadas por el runtime, misma respuesta en lista, agenda y editor |
| Tasks empieza con el objetivo | Nuevo chat listo para escribir, responsable opcional | General por defecto; no pedir nombre/rol/modelo antes de poder empezar |
| Hilo por tarea | Conversación independiente para cada encargo | Continuar conserva ese trabajo. Otro encargo crea otro hilo aunque use el mismo agente |
| Routines muestra el resultado en cada paso | Resultado y estado accesibles desde el nodo | Separar Editar de Historial para que inspeccionar un resultado no modifique el plan |
| Agenda y próximas ejecuciones | Vista Agenda dentro de Automatizaciones | Proyección autoritativa, distingue previsto, omitido y realmente ejecutado |
| App interactiva en la landing | Demo ARK con un caso completo | Datos de ejemplo explícitos, reiniciable, sin cuenta, sin llamadas reales |
| Muchas herramientas de canvas | Seleccionar, mover, conectar, ajustar vista | No añadir dibujo, figuras y notas antes de resolver ejecución y recuperación |

No reproducir pantallas, textos, marca, paleta ni composición de Openbot.
Resolver estos trabajos con los objetos y tokens de ARK.

## Navegación y nombres

| Lugar | Trabajo del usuario | Contenido |
|---|---|---|
| Nuevo chat, arriba del sidebar | Empezar algo | Composer, archivos, proyecto actual y responsable opcional |
| Chats, en proyectos | Retomar conversación | Historial existente, búsqueda y agentes persistentes |
| Trabajo, en navegación | Ver lo que ocurre y lo que necesita mi respuesta | Requiere tu atención, En curso, En espera, Completado; filtros por proyecto y origen |
| Automatizaciones, reemplaza Schedules | Preparar y repetir trabajo | Lista por defecto; Agenda como vista; editor con Pasos y Canvas |
| Agentes | Configurar colaboradores | Identidades, instrucciones y relaciones existentes. No es requisito para empezar |
| Plugins y ajustes | Conectar capacidades y preferencias | Se conservan. Webhook y cron aparecen solo cuando se eligen sus opciones técnicas |

No convertir todos los chats históricos en tareas. Un chat general puede tener varios encargos.
Trabajo proyecta encargos con identidad durable, no cada burbuja ni cada paso técnico.
Una ejecución automática aparece en Trabajo y en el historial de su automatización: mismo ID.
El proyecto fija la pertenencia. Un filtro Todos puede agregar proyectos sin cambiar el origen.
En cada fila agregada se ve el proyecto y el click lleva a ese origen.

## Flujos completos

### 1. Empezar y continuar

1. Nuevo chat o Cmd+N abre un borrador enfocado. No crea agente, tarea ni conversación vacía.
2. Se puede escribir y adjuntar archivos inmediatamente. Usa proyecto actual y General.
3. El selector opcional permite otro agente existente. Crear agente queda como acción secundaria.
4. Enviar conserva borrador/adjuntos hasta recibir una admisión durable con un ID estable.
5. El backend crea o encuentra la conversación y el trabajo de ese envío, con recibo de reintento.
6. Se abre el chat, el título se deriva del objetivo y Trabajo muestra el estado autoritativo.
7. Un segundo mensaje en ese chat continúa su contexto. Nuevo chat empieza otro trabajo.
8. Un chat directo de agente sigue siendo accesible desde su identidad. Iniciar otro encargo
   desde allí crea hilo separado; no renombra ni borra su conversación permanente.

No depender de un modelo para admitir, titular provisionalmente, elegir General o crear el hilo.
Delegar el trabajo sí conserva el comportamiento existente del agente y sus aprobaciones.
Una tarea terminada recibe seguimiento en su conversación; no se vuelve a ejecutar sola.
Si el seguimiento necesita otro TaskRecord, queda enlazado al mismo encargo visible.

### 2. Encontrar resultados y responder

Trabajo muestra primero preguntas y aprobaciones, luego en curso y en espera.
Completado contiene respuestas terminadas, no todas bajo Requiere tu atención.
Abrir fila muestra conversación, archivos y resultado. Un inspector ofrece pasos y costes reales.
Responder una aprobación usa su ID existente. La fila desaparece de Atención cuando se resuelve.
Cancelar interrumpe el trabajo real. Archivar oculta de la lista personal, no borra los resultados.
Reintentar explica si continúa o crea una nueva ejecución. Nunca repite un efecto incierto.

### 3. Convertir trabajo en automatización

1. Menú del chat o resultado > Guardar como automatización.
2. Prepara un borrador: objetivo, entradas explícitas, archivos referenciados y responsable.
3. No copia el historial entero, secretos ni resultados efímeros como instrucciones permanentes.
4. Editor compacto: Inicio > Trabajo > Resultado. Manual por defecto.
5. Elegir Horario abre frecuencia/días/hora/zona en el primer bloque. Cron es opción avanzada.
6. Se ve la próxima ejecución y una vista previa de entradas y efectos autorizados.
7. Guardar conserva borrador. Activar requiere la aprobación vigente de trabajo desatendido.
8. Ejecutar ahora crea una ejecución identificable, abre su detalle y conserva la próxima fecha.

### 4. Construir varios pasos

Empezar con la misma automatización simple y añadir pasos. Pasos y Canvas editan el mismo grafo.
Cada paso tiene entrada, acción y salida identificables. El conector ofrece solo puertos compatibles.
Agregar después de un nodo conecta automáticamente. El arrastre es opcional.
Seleccionar nodo abre propiedades; los cambios breves de disparador ocurren inline.
La versión comprometida es una secuencia de hasta8pasos. Una aprobación pausa antes del siguiente paso.
Las condiciones y ramas paralelas son una extensión posterior, sin controles ficticios en v1.
Historial superpone estados/resultados de una ejecución sobre su revisión congelada.
Volver al editor recupera el borrador actual sin trasladar resultados a sus campos.

### 5. Webhook

Al recibir un evento > Webhook abre configuración avanzada del primer bloque.
Se elige una conexión/entrada HTTPS ya disponible, se prueba un evento sintético y se mapean campos.
La dirección y el secreto se generan solo al configurar explícitamente esa entrada.
La interfaz dice Pendiente de conexión si el runtime no es alcanzable.
No crea túneles, abre puertos ni exige Cloud para ejecutar manualmente o por horario.
Activar muestra qué evento lo inicia, datos accesibles, efectos y límites.
El payload es dato no confiable. No puede ampliar permisos ni editar el flujo.
Recepción, rechazo, duplicado y ejecución quedan visibles sin registrar secretos.

## Contratos de datos antes de construir UI

Nombres siguientes: propuesta de contrato, no APIs existentes.
Resolver el esquema final y documentarlo antes del primer componente.

| Entidad | Campos mínimos o referencia | Dueño y compatibilidad |
|---|---|---|
| WorkView | id, project, conversation_ref, root_task_ref opcional, origin, title, state, attention_refs, timestamps, output_refs | Proyección en ark-interact. No copiar TaskRecord como segundo estado editable |
| Conversation reference | General existente, agente persistente o hilo de encargo con ID estable | Reutilizar autoridad de conversación actual. Si falta hilo aislado, añadir binding durable; no simularlo filtrando por nombre de agente |
| Submission receipt | project, origin/request key, canonical digest, conversation_ref, work_ref | Admisión atómica Core. Mismo envío devuelve mismo resultado; otra carga con misma clave se rechaza |
| AutomationDefinition | id, project, revision, name, draft/published, owner_chat_ref, trigger_ref, graph_ref, policy_ref | Store/orchestrate. Nueva entidad solo donde no alcanza el schedule existente |
| WorkflowRevision | id, immutable revision, nodes, edges, input schema, output bindings | Grafo ejecutable acíclico en Core, no coordenadas de pantalla |
| WorkflowLayout | definition/revision ref, node positions, viewport | Metadatos de presentación separados del grafo. No afectan ejecución |
| Trigger | Manual; Time referencia ScheduleRecord; Event referencia EventAutomationRecord | Adaptadores de un contrato común. Mantener IDs y JSON de horarios existentes |
| Node | id estable, kind, configuration, typed input/output bindings | v1: Inicio, Agente, Aprobación, Resultado. Sin paso LLM paralelo a Agente |
| Run | id, definition revision, trigger key/slot, root task, per-node task/approval/output refs | Reutilizar tareas, retry, atención y resultados. Estado derivado de esos registros |

Fuente existente:
- `crates/ark-orchestrate/src/spec.rs:239`: TaskSpec ya lleva objetivo, contexto, artefactos,
  dependencias `after`, budget, reintentos y `report_to`.
- `crates/ark-orchestrate/src/record.rs:268`: TaskRecord ya tiene proyecto, owner, estado,
  children, inputs/outputs, modelo, timestamps, schedule, slot y result.
- `crates/ark-interact/src/orchestration.rs:97`: ScheduleView; `:154`: schedule_calendar.
- `crates/ark-ui/web/js/schedules.js:56`: lista/calendario y control por proyecto.
- `../ark-desktop/ui/desktop.js:80`: entrada determinista por mención usa submit_task.
- `../ark-desktop/ui/agent-chat.js:439`: conversación del agente envía submit_task.
- `../ark-desktop/ui/agents.js`: canvas existente de agentes, no editor de procesos persistidos.

Conservar ScheduleRecord como autoridad del reloj, sin segundo scheduler JS o cron de Cloud.
Para ejecutar un grafo desde horario, añadir una referencia de destino opcional con default compatible,
validada en Core, o binding durable equivalente. La alternativa elegida debe resolver atómicamente
schedule-slot -> revisión -> ejecución. No usar un prompt oculto que ordene al modelo interpretar nodos.
Una secuencia publicada puede generar tareas mediante TaskSpec.after.
Eso no resuelve por sí solo aprobación, publicación, reinicio o deduplicación:
implementarlos como coordinador durable del flujo, sin duplicar el loop de agentes.


### Agrupación durable de Trabajo

Decisión propuesta: WorkItem existente es el encargo visible. `TaskSpec.item` enlaza cada ejecución
con ese encargo. `TaskSpec.work` conserva su significado de procedencia de handoff, no se reutiliza
como agrupador. `WorkView.id/work_ref` es el ID del WorkItem, no un ID de pantalla nuevo.
El board existente puede representarlo; no se crean dos tarjetas ni otro estado de tareas.
Añadir el binding mínimo item -> conversation_ref y metadatos archivado por usuario al store,
con admisión atómica junto al recibo. La tarea inicial y los seguimientos conservan el mismo item.

Atención se deriva de approvals abiertas. En curso de tareas abiertas; En espera de su motivo.
Sin tareas abiertas, un fallo no resuelto prevalece sobre éxito. Completado requiere resultado final.
WorkItem sigue sus reglas de estado actuales; la proyección detallada no las sobrescribe.
Guardar refs de resultados y un resumen durable al terminar cada tarea del encargo.
La retención de schedules ya conserva tareas vinculadas a items; verificar ese contrato en P0.
No ampliar retención de todos los eventos sin límite: lista paginada y límites existentes.
Archivar es metadato recuperable, no borrado. Eliminar conversación sigue el contrato actual,
no elimina implícitamente el encargo ni sus salidas. Una referencia eliminada se muestra como tal.
Prueba obligatoria: dos tareas de un mismo encargo, reinicio, archivo/desarchivo y poda de otra
schedule no vinculada. Siguen existiendo conversación, agrupación y resultados del encargo.

### Autoridad del webhook de principio a fin

P0 debe demostrar exclusión de segundo writer, dedupe durable y cutover de activación antes de UI.
P4 debe congelar revisión, entradas y autoridad en la ejecución; cada tarea hija y delegada hereda
una restricción igual o más estrecha. Los reintentos conservan esa autoridad y sus contadores.
P6 verifica la autoridad antes de cada acción, incluidas lecturas, procesos y red.
Los outputs intermedios siguen siendo datos no confiables: no alteran permisos, rutas permitidas,
identidad del evento ni configuración. Cuotas se consumen con operación durable y atómica.
Agotarlas pausa o rechaza el paso, sin resetearlas al reintentar o reiniciar.
Activar o cambiar crea watermark/revisión: eventos anteriores al corte no entran como nuevos.
Nunca reinterpreta un evento ya admitido con una revisión posterior del flujo.
Estas son dependencias de seguridad de P6, no características opcionales del primer webhook.
Reusar A0/A1 del board (ea61a000/ea61a001) en vez de duplicar su implementación.
Ambas tarjetas siguen todo: sus notas hablan de fuente preparada pendiente de integración,
aceptación de ADR0062 y verificación del candidato. No son dependencias aprobadas en main.
P0 debe medir compatibilidad de ramas, esfuerzo de integración y checks pendientes.
El alcance Gmail/WhatsApp de AUTOMATIONS.md no se implementa en estos sprints: cualquier
cambio a su requisito de fixtures necesita aceptación explícita de la enmienda ADR0062.

## Reglas de ejecución y edición

| Caso | Contrato v1 |
|---|---|
| Editar mientras corre | La ejecución usa su revisión congelada. Publicar afecta solo nuevas ejecuciones |
| Borrador inválido | Se puede guardar; Activar/Ejecutar desactivado con error junto al nodo |
| Grafo | Un inicio por definición; secuencia acíclica, máximo8pasos. Una salida por paso y sin joins, condiciones ni código libre |
| Cambio de disparador | No convierte automáticamente una entidad activa. Pausar/publicar reemplazo y recibo atómicos; jamás dos triggers activos por un fallo parcial |
| Schedule legado | Representación simple de un paso, mismo ID y autoridad. Ver/abrir no escribe ni migra. Editar grafos crea binding explícito, no conversión silenciosa |
| Duplicado | Claves durables de envío, slot o evento. Una sola admisión; no prometer efectos externos exactamente una vez |
| Fallo parcial | Conserva salidas completadas y explica desde dónde puede continuar. No vuelve a enviar efectos inciertos |
| Aprobación | El nodo explícito es un checkpoint del plan. No sustituye las aprobaciones de las acciones que el agente intente |
| Pausar | Detiene nuevos inicios. Una ejecución trabajando o esperando aprobación sigue; un schedule esperando reintento se cancela, conforme ADR0053. Cancelar ejecución es otra acción |
| Agente eliminado/modelo no disponible | Error recuperable antes de comenzar el paso. No sustitución silenciosa ni cambio de cuenta |
| Reconexión | Releer snapshot y recibo por ID; jamás reenviar por creer que un spinner equivale a fallo |
| Archivar | Estado de presentación recuperable por usuario, no borrado de registros de trabajo |
| Webhook | HTTPS existente, firma/verificación, ventana anti-replay, límite de tamaño/tasa, clave evento durable y autoridad por ejecución |
| Evento de prueba | Preview de mapeo y validación sin efectos. Ejecutar con ese ejemplo requiere acción explícita |

## Estados y calidad de interacción

| Estado/superficie | Comportamiento requerido |
|---|---|
| Vacío Nuevo chat | Composer enfocado y proyecto visible; no formulario previo |
| Vacío Trabajo | Composer compartido y acción para empezar; no texto que repita sus controles |
| Vacío Automatizaciones | Crear automatización; dos ejemplos locales editables y etiquetados como ejemplos |
| Cargando | Contenedor estable y feedback inmediato; no lista que se borra con cada actualización |
| Error/refusado | Conservar texto, archivos, conexiones y selección. Mensaje concreto con reintento o corrección |
| Offline | Último estado con marca de desactualizado; editar borrador permitido, activar/enviar espera conexión |
| Guardado | Diferenciar Guardando, Guardado, Error y conflicto de revisión. Nunca afirmar guardado antes del recibo |
| Cambio de proyecto/computadora | Cancelar observaciones viejas. No mezclar borradores, resultados o admisiones de otro origen |
| Teclado | Cmd+N, foco y Escape coherentes; conectar y reordenar desde Pasos; no canvas obligatorio |
| Small window/touch | Pasos es equivalente funcional; panel a pantalla completa. Hit targets40px o mayores; teléfono requiere prueba física |
| Light/dark | Tokens de BRAND.md. Estado con texto/forma además del color. Contenido largo no rompe filas/nodos |
| Movimiento | Feedback inmediato; menú150ms entrada/100ms salida, panel200ms. Teclado inmediato y reduced-motion sin movimiento |
| Canvas | Pan/zoom conservan ancla y selección; encuadrar no aleja todo al agregar un paso. Sin animación en cada actualización |
| Tiempo | IANA visible al editar. Próximas fechas calculadas en Core, incluyendo DST. Nunca estimar agenda con JS independiente |

Diagnóstico Norman del flujo actual Schedules vacío: evaluación de diseño, no test con usuarios.
6/10: estado visible2, recuperación2, constraints2; descubrimiento0 (no crear aquí), mapping0
(edición ocurre en otro lugar). Objetivo10/10, que debe establecerse con pruebas de uso.
No hay puntuación de usabilidad general de ARK ni de Openbot a partir de este recorrido.

## Dos sprints para Terra

Supuesto de planificación: dos sprints de10días hábiles, un implementador Terra, QA independiente.
Los días son capacidad estimada, no garantía ni fecha de release. Compromiso funcional acotado:
secuencia8pasos, aprobaciones y un webhook genérico; sin ramificaciones ni proveedores de eventos.
No aceptar un canvas de muestra como implementación completa.

| ID | Sprint/orden | Entrega verificable | Estimación |
|---|---|---|---|
| P0 | S1 día1-2 | Contratos, mapa de revisiones reales, enmiendas ADR aceptadas antes de cambiar contratos y prueba vertical: recibo -> hilo aislado -> resultado; writer/cutover/autoridad en un evento sintético | 2d |
| P1 | S1 trasP0 | Nuevo chat con General por defecto, agente opcional y seguimiento separado del chat persistente del agente | 2d |
| P2 | S1 trasP1 | Trabajo por estado, conversación/resultado/aprobaciones, reintento y cancelación usando autoridad existente | 2d |
| P3 | S1 trasP0 | Automatizaciones lista/agenda, horarios legados, editor Inicio/Trabajo/Resultado con creación/cambio estructurado y aprobación | 2d |
| Q1 | S1 cierre | Gate completo Core/Desktop afectado, migraciones/recibos/reconexión y dogfood real. Revisión de contratos/diseño antes de construir | 2d |
| P4 | S2 día1-3 | Secuencia publicada/revisiones, autoridad transitiva y pasos Agente/Aprobación/Resultado, con recuperación | 3d |
| P5 | S2 día4-5 | Canvas y Pasos sobre la misma secuencia, conectores tipados, inspector y modo Historial; reordenar/undo de edición | 2d |
| P6 | S2 día6-7 | Webhook genérico autenticado por entrada HTTPS existente, preview/mapeo, dedupe y autoridad de ejecución | 2d |
| P7 | S2 día8 | Demo local EN/ES de un recorrido fijo, con botones de aprobación/reset; automatizar abre ejemplo simple | 1d |
| Q2 | S2 día9-10 | Casos integrados, QA independiente, correcciones, app real, documentación y entrega local revisable | 2d |

P3 no depende del canvas. P4/P6 dependen de P0. P0 entrega una matriz go/no-go con
recibo/conversación, retención WorkItem, schedule-destino, writer/cutover y autoridad transitiva.
Cada fila requiere un test ejecutable o diagnóstico concreto, responsable y estimación revisada.
Go exige que esos contratos puedan completarse dentro de sus unidades de la tabla. Si no,
se declara no-go del compromiso de dos sprints antes de comenzar UI dependiente.
El plan no convierte incertidumbre en promesa de calendario.

Reserva real: P7 puede sustituirse por una demo de tres estados y reset (medio día), P5 puede
omitir undo global y conservar revertir campo/reordenar (medio día). Se libera hasta1día para
correcciones; no se recortan autoridad, recuperación, webhook ni accesibilidad.
Condiciones, ramas paralelas, loops, dibujo, minimapa y mes nuevo son extensiones excluidas,
no supuestos recortes futuros. No gastar reserva en ellas.

### Aceptación por entrega

| ID | Prueba humana y automática mínima |
|---|---|
| P0/P1 | Dos envíos con misma clave crean un encargo. Dos encargos al mismo agente tienen hilos distintos. Abrir Nuevo chat y cancelar no deja agente/chat/tarea vacíos. Sin modelo disponible el borrador sigue intacto |
| P2 | Un trabajo que espera permiso está en Atención. Resolver desde otro cliente lo actualiza. Respuesta terminada va a Completado. Abrir resultado y volver conserva filtros/posición |
| P3 | Crear horario sin pedir al modelo que traduzca el formulario. Cambiar/pause/resume/run now con recibo. Cron inválido y zona distinta muestran corrección y próximas3fechas. Calendario concuerda con detalle |
| P4 | Secuencia: salidas solo llegan al paso siguiente. Aprobación detiene downstream. Reinicio en cada frontera no duplica ejecución ni pierde respuesta. Tarea hija/delegada/reintentada conserva autoridad; cuotas no se reinician |
| P5 | Crear flujo completo sin arrastrar; Canvas y Pasos coinciden. Undo/redo del borrador no altera revisión publicada. Si se usa reserva, revertir campo y reordenar sustituyen undo global con la misma garantía. Abrir historia no modifica definición |
| P6 | Evento válido llega una vez; duplicado, firma mala, replay, oversize y rate-limit tienen resultado explícito. Inyección en payload no amplía autoridad. Sin HTTPS alcanzable no aparece como activo. Eventos anteriores al cutover no corren bajo revisión nueva; grants amplios y --yes no amplían autoridad; aliases de rutas/red/procesos se rechazan |
| P7 | Persona puede completar demo sin cuenta. Ejemplo claramente identificado, reset, sin llamadas reales ni upload. Funciona con teclado y móvil; sin JS permanece una explicación estática útil |
| Q2 | Dogfood con backend real: manual, horario y webhook llegan al mismo detalle de ejecución. Cancelación/reconexión/proyecto distinto/dark/ventana pequeña pasan. Pruebas con modelo sintético se etiquetan, no prueban calidad real |

### Qué queda fuera de estos dos sprints

Condiciones y bifurcaciones paralelas, marketplace de plantillas, editor de código libre, bucles,
subflows recursivos, colaborativo multiusuario,
multiplayer del canvas, túneles públicos automáticos, integración completa Gmail/WhatsApp,
provisión Cloud, nuevos SDKs de canvas, rediseño total del sitio, analítica nueva y editor nativo iPhone.
El teléfono conserva leer/responder aprobaciones/resultados por su protocolo existente.
Nuevas mutaciones nativas requieren una unidad posterior de paridad con fixtures generados.
No prometer activación en dos sprints si las fronteras de ejecución siguen sin resolver.

## Dueños, ADRs y pruebas de release

| Capa | Cambio |
|---|---|
| ark-store / ark-orchestrate | Bindings durables, revisiones/grafo, ejecuciones y deduplicación. Reusar TaskSpec/TaskRecord y ScheduleRecord |
| ark-tools / ark-connect | Contrato neutral de evento y verificación en borde, siguiendo ADR0062. No depender del consumidor |
| ark-interact | Proyecciones Trabajo/Automatizaciones, estados, permisos, comandos para CLI y GUI |
| ark-ui | Host/API y shell compartido, nuevo inicio, bandeja, editor/agenda y webhook endurecido |
| ark-desktop | Configuración de agentes/identidad y adaptación al shell. Reutilizar primitivas de canvas sin convertir identidad en paso |
| ark-site | Demo estática interactiva con estado sintético, locales EN/ES y claims acordes al release |
| ark-ios | Sin editor nuevo en este alcance. Verificar que cambios aditivos no rompen clientes anteriores |

ADRs a preparar: ampliación de0007 (composición directa opcional), ampliación de0053
(edición estructurada por persona y destino de ejecución), revisión de0062 propuesto (webhook
mínimo y autoridad), revisión de0092 (identidad persistente frente a hilo de encargo),
uno nuevo para revisiones ejecutables del workflow. No marcar aceptados por haber escrito este plan. Su aceptación registrada es requisito para
P1/P3/P4/P6 que cambien esas fronteras. P0 puede investigar y presentar la propuesta antes de
esa decisión. La petición de análisis no equivale a aceptar enmiendas arquitectónicas.
No cambiar silenciosamente las reglas que hoy dicen que se crea/edita solo hablando.

Cada unidad usa vstack; data shape primero. Un propietario por árbol, worktree cuando hay editor.
QA de plan y diseño antes de código; QA de implementación antes de entrega.
Core `scripts/ci.sh`; Desktop `scripts/check.sh`; Site `node src/build.mjs --check`, uno a la vez.
Protocolos/fixtures si cambian; benchmark simulado antes/después si cambia lo que recibe un modelo.
Dogfood sobre app real con revisión identificada y capturas de cada recorrido.
Build/firmado/instalación/publicación son gates separados; no autorizados por este plan.
Core primero, release/tag solo cuando el dueño lo pide, consumidor después.

## Landing y validación con personas

Mantener headline actual, fotos, papel/tinta/azul y el caso de una persona trabajando.
Cambiar See how it works para abrir una demo ARK controlable en la propia página.
La demo enseña un resultado útil antes de mostrar un grafo. Automatizar es el siguiente paso opcional.
Mostrar estados reales de producto en versión demo con etiqueta Ejemplo; lo futuro se indica como tal.
No copiar el hero con brillo violeta ni agrandar el menú para igualar la referencia.

Prueba prevista, sin inventar resultados: cinco personas nuevas, tres tareas, sin instrucciones.
A: iniciar un encargo sin crear agente. B: convertirlo en horario. C: encontrar y responder una aprobación.
Medir éxito, ayudas y tiempo. Objetivos de aceptación:5/5 inician sin ayuda,4/5 configuran horario
correcto en menos de2minutos,5/5 ubican una aprobación y distinguen Guardado de Activo.
Esos umbrales son decisiones de prueba, no métricas observadas.
Para landing medir comprensión en esas sesiones antes de instrumentar datos o lanzar un A/B.
No contratar personas, enviar invitaciones ni instalar analítica por esta propuesta.

## Handoff para Terra

Implementar P0 a Q2 en ese orden y alcance, con dos sprints máximos como presupuesto de planificación.
Antes de editar: leer este plan, AGENTS/WORKSPACE, estado y ADRs actuales, board y revisiones
instaladas/árboles de cada repo. El plan cita main0.30 mientras la observación instalada era0.31:
resolver esa diferencia y reaprovechar cambios ya entregados, sin reimplementarlos.
No usar la demo del competidor como especificación del backend.
Preparar contrato y estados de cada unidad y pasar QA antes de implementar.
No abrir chats nuevos, elegir modelo para otra conversación ni iniciar implementación desde esta investigación.
La selección de Terra para la implementación futura es del dueño y ya está indicada en el brief.

## Revisión del plan

QA independiente qa_openbot_plan: PASS tras corregir alcance, autoridad y agrupación.
El verdict valida este plan acotado y sus gates, no prueba factibilidad de20días ni implementación.
La integración A0/A1 y las enmiendas ADR siguen pendientes del gateP0.
