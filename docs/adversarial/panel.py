"""Levanta una instancia aislada con datos hostiles para probar el panel (máx. 6 min)."""
import os, time, signal, shutil
import ataques as A
e = A.Env("panel")
P = '<img src=x onerror="window.__xss=(window.__xss||[]).concat(\'%s\')">'
open(e.repo + "/<img src=x onerror=window.__xss=['archivo']>.txt", "w").write(P % "contenido")
open(e.repo + "/README.md", "w").write("original\n")
e.git("add", "-A"); e.git("commit", "-qm", "hostil")
os.makedirs(e.root + "/<img src=x onerror=window.__xss=['carpeta']>")
os.makedirs(e.root + "/otro"); open(e.root + "/otro/README.md", "w").write("otro\n")
e.start(port=7793)
try:
    e.settings(orchestrator_agent="claude", agent_models={"claude": P % "modelo"})
    open(e.root + "/orq_reply.txt", "w").write(P % "respuesta")
    e.req("POST", "/api/orchestrator/chat", {"message": P % "chat"})
    e.task(P % "prompt", agent="mock", titulo=P % "titulo")
    e.req("PUT", "/api/quota", {"providers": {"copilot": {"currency": '"><img src=x onerror="window.__xss=1">'}}})
    print("listo", e.root, flush=True)
    time.sleep(360)
finally:
    e.stop(); shutil.rmtree(A.LAB, ignore_errors=True)
