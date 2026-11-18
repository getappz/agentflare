import contextlib
import io
import json
import sqlite3
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

import distraction_baseline as baseline


class BaselineTest(unittest.TestCase):
    def test_classification_uses_project_and_prompt_at_action_time(self):
        with tempfile.TemporaryDirectory() as tmp:
            db = Path(tmp) / "sessions #1.db"
            with contextlib.closing(sqlite3.connect(db)) as c:
                c.executescript("""
                    create table projects(id, name, deleted_at);
                    create table items(project_id, sequence_id, state_id, deleted_at);
                    create table states(id, group_name);
                    insert into projects values (1,'a',null), (2,'b',null);
                    insert into states values (1,'completed'), (2,'started');
                    insert into items values (1,7,1,null), (2,7,2,null);
                    create table sessions(id,project,model,tool_call_count,duration_secs,
                        input_tokens,output_tokens,cache_read_tokens,cache_write_tokens,started_at);
                    insert into sessions values (1,'b','m',5,1,0,1,0,0,'2026-01-01'),
                                                (2,'b','m',1,1,0,1,0,0,'2026-01-01');
                    create table turns(session_id,seq,user_text);
                    insert into turns values (1,1,'Handle #12.'), (1,2,'Now check #7.');
                    create table tool_calls(session_id,turn_seq,name,input,output,created_at);
                    insert into tool_calls values
                        (1,1,'Read','','Item #7/#12; TODO(#3)',1),
                        (1,1,'Bash','git log -3','',2),
                        (1,1,'Bash','agentflare item get 7','',3),
                        (1,1,'Bash','gh issue view 12','',4),
                        (2,1,'Read','','no refs',1);
                """)
            args = ['baseline', '--db', str(db), '--json']
            output = io.StringIO()
            with patch.object(sys, 'argv', args), patch.object(
                baseline, 'item_states', return_value=baseline.item_states(db)
            ), contextlib.redirect_stdout(output):
                baseline.main()
            report = json.loads(output.getvalue())
            self.assertEqual(report['refs_exposed'], 3)
            self.assertEqual(report['refs_acted'], 2)
            self.assertEqual(report['unprompted_detours'], 1)
            self.assertEqual(report['detour_open'], 1)
            self.assertIsNone(report['matched_overhead_steps'])


if __name__ == '__main__':
    unittest.main()
