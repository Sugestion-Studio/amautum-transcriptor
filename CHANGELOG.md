# Novedades

Qué cambia en cada versión, contado para quien **usa** el programa.

Esto no es el historial de commits: ahí queda el detalle técnico, con su porqué,
para quien mantiene el código. Aquí solo entra lo que le cambia algo a la persona
que instala la aplicación. Un arreglo de CI o una advertencia del compilador son
importantes, pero no en esta página.

Cada versión responde a tres preguntas: qué se arregló, qué es nuevo y qué hay
que saber para actualizar.

---

## 0.1.13

### Arreglado

- **La ventana de estado no mostraba nada.** El programa transcribía bien, pero
  su ventana no lograba comunicarse con el motor y aparecía siempre vacía o con
  el mensaje «no responde». Ahora muestra el progreso real: en qué tramo va,
  cuánto lleva, cuánto falta y hace cuánto dio señales el motor.

- **Un audio que todavía se estaba copiando podía transcribirse a medias.** Al
  elegir un archivo desde una unidad de red o un pendrive antes de que terminara
  de copiarse, la transcripción salía cortada y parecía correcta. Ahora el
  programa lo detecta antes de empezar y avisa.

### Nuevo

- **Ayuda a un clic desde cualquier sitio.** El icono de la bandeja, el menú y la
  ventana llevan a soporte. El ticket llega con la versión, el sistema y el
  último error ya puestos: solo hay que contar qué pasó.

- **«Acerca de» con información útil.** Qué hace el programa, qué versión tienes
  y a dónde acudir, en vez de solo un número.

### Al actualizar

Nada que hacer. Si vienes de la 0.1.12, la actualización se instala sola cuando
el programa esté libre de trabajo.

---

> Las versiones anteriores a la 0.1.13 no tienen notas aquí: este archivo
> empieza a llevarse desde ella.
