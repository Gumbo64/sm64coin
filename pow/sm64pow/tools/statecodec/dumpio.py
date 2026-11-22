"""Reader for statedump output (see examples/statedump.rs)."""
import numpy as np
COLS = ['action','prev_action','flags','input','action_state','action_timer','intended_yaw','intended_mag','face_yaw','cam_yaw',
        'px','py','pz','fwd','vx','vy','vz','health','level','area','timer','buttons','sx','sy','poked']

def load(path):
    b = open(path, 'rb').read(); i = 0; runs = []
    while i < len(b):
        n = int.from_bytes(b[i:i+4], 'little'); name = b[i+4:i+4+n].decode(); i += 4 + n
        r = int.from_bytes(b[i:i+4], 'little'); i += 4
        a = np.frombuffer(b[i:i+r*100], dtype='<i4').reshape(r, 25); i += r*100
        runs.append((name, a))
    return runs
