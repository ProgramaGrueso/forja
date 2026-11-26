# Skills de Forja

Skills genéricas (formato estándar `SKILL.md` con frontmatter `name`/`description`, compatibles con Claude Code, OpenCode, Agy y Hermes) que `forja skills install` copia a la carpeta de cada agente. Se embeben en el binario al compilar (`crates/forja-cli/build.rs`); cada carpeta de aquí es una skill y se incluye sola si contiene `SKILL.md`. Este `README.md` no se embebe.

Curación: se partió de las skills de los distintos agentes de uso del autor, se descartaron las que tenían contenido personal o de proyectos privados, las atadas a una herramienta concreta (scripts, rutas o APIs propias de un agente) y las de licencia indeterminada, y se limpiaron nombres, rutas y referencias específicas. Los nombres de herramienta en los ejemplos (`terminal(...)`, `read_file`, `delegate(...)`) son pseudocódigo: cada agente usa su equivalente.

## Procedencia

| Skill | Origen | Autor | Licencia |
|---|---|---|---|
| systematic-debugging | Skills de Hermes Agent (Nous Research), adaptada de obra/superpowers | Hermes Agent; obra/superpowers | MIT |
| test-driven-development | Skills de Hermes Agent, adaptada de obra/superpowers | Hermes Agent; obra/superpowers | MIT |
| requesting-code-review | Skills de Hermes Agent, adaptada de obra/superpowers y MorAlekss | Hermes Agent; obra/superpowers; MorAlekss | MIT |
| simplify-code | Skills de Hermes Agent, inspirada en `/simplify` de Claude Code | Hermes Agent | MIT |
| spike | Skills de Hermes Agent, adaptada de gsd-build/get-shit-done | Hermes Agent; Lex Christopherson (GSD, © 2025) | MIT |
| github | Skills de Hermes Agent (reducida a flujos con `gh`; sin scripts de credenciales) | Ben Barclay (benbarclay); Hermes Agent | MIT |
| codebase-inspection | Skills de Hermes Agent | Hermes Agent | MIT |
| python-debugpy | Skills de Hermes Agent (sin partes específicas de Hermes) | Hermes Agent | MIT |
| node-inspect-debugger | Skills de Hermes Agent (sin partes específicas de Hermes) | Hermes Agent | MIT |
| dogfood | Skills de Hermes Agent (genérica: sin herramienta de navegador propia) | Teknium (teknium1); Hermes Agent | MIT |
| architecture-diagram | Skills de Hermes Agent, portada de Cocoon AI | Cocoon AI; Hermes Agent | MIT |
| forja-orquestador | Forja | Proyecto Forja | Igual que el repositorio |
| forja-revision | Forja | Proyecto Forja | Igual que el repositorio |
| writing-plans | Forja | Proyecto Forja | Igual que el repositorio |
| subagent-delegation | Forja | Proyecto Forja | Igual que el repositorio |

La licencia de cada skill importada se tomó del `license:` de su frontmatter original (todas `MIT`). Se quitaron `author`/`metadata` del frontmatter para que sea portable; la atribución queda en esta tabla. Se descartaron, entre otras, las skills sin licencia declarada o con licencia propietaria (p. ej. las sincronizadas por Anthropic con `LICENSE.txt` propietario) y las de otros ámbitos (documentos ofimáticos, correo, medios, escritorio, proyectos privados).

## Licencia MIT de las skills importadas

Permission is hereby granted, free of charge, to any person obtaining a copy of these skills and associated documentation files (the "Software"), to deal in the Software without restriction, including without limitation the rights to use, copy, modify, merge, publish, distribute, sublicense, and/or sell copies of the Software, and to permit persons to whom the Software is furnished to do so, subject to the following conditions:

The above copyright notice (those of the authors listed in the table) and this permission notice shall be included in all copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE SOFTWARE.
