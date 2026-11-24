import re
s=open('sessions.rs').read()
# (op, json fn, params type, handler call, result kind)
simple=[('stop_session_json','StopSessionParams','stop_session'),
('sweep_sessions_json','SweepSessionsParams','sweep_sessions'),
('log_tail_json','LogTailParams','log_tail'),
('input_json','InputParams','input'),
('screen_json','ScreenParams','screen'),
('wait_json','WaitParams','wait'),
('wake_json','WakeParams','wake'),
('keep_awake_json','KeepAwakeParams','keep_awake'),
('set_role_json','SetRoleParams','set_role'),
('answer_json','AnswerParams','answer'),
('hibernate_json','HibernateParams','hibernate')]
impls=[]
typed=[]
for fn,P,h in simple:
    m=re.search(r'fn %s<C, I>\(ctx: &AppContext<C, I>, params: Value\) -> Result<Value, VogtError>\nwhere\n    C: Clock \+ \'static,\n    I: IdFactory \+ \'static,\n\{\n    let parsed = %s \{\n(.*?)\n    \};\n    (.*?)\n\}\n'%(fn,P),s,re.S)
    assert m,fn
    body=m.group(1)
    impls.append("impl<'de> serde::Deserialize<'de> for %s {\n    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {\n        from_wire(deserializer, |params| {\n            Ok(Self {\n%s\n            })\n        })\n    }\n}\n"%(P,body))
    newfn="fn %s_typed<C, I>(ctx: &AppContext<C, I>, parsed: %s) -> Result<%s, VogtError>\nwhere\n    C: Clock + 'static,\n    I: IdFactory + 'static,\n{\n    %s(ctx, &parsed)\n}\n"%(h,P,'Value' if h=='answer' else 'SessionResult',h)
    s=s.replace(m.group(0),newfn)
open('sessions.rs','w').write(s)
open('/tmp/impls_w4618.rs','w').write("\n".join(impls))
