// Windows-only UI for the IndustrialRust1710 CLI server.
// The server remains the authority: this process only displays stdout/stderr and
// forwards administrator commands to its standard input.
using System;
using System.Collections.Generic;
using System.Diagnostics;
using System.Drawing;
using System.Text;
using System.Text.RegularExpressions;
using System.Windows.Forms;

namespace IndustrialRust1710
{
    public sealed class ServerGui : Form
    {
        private readonly string executable;
        private readonly string[] arguments;
        private readonly string directory;
        private readonly Queue<string> pending = new Queue<string>();
        private readonly HashSet<string> online = new HashSet<string>(StringComparer.OrdinalIgnoreCase);
        private readonly List<long> workingSetHistory = new List<long>();
        private readonly System.Windows.Forms.Timer refreshTimer = new System.Windows.Forms.Timer();
        private readonly RichTextBox log = new RichTextBox();
        private readonly TextBox command = new TextBox();
        private readonly Button send = new Button();
        private readonly Button stop = new Button();
        private readonly ListBox players = new ListBox();
        private readonly PictureBox graph = new PictureBox();
        private readonly Label stats = new Label();
        private readonly ToolStripStatusLabel status = new ToolStripStatusLabel();
        private Process server;
        private bool stopping;
        private bool closing;
        private bool exitObserved;
        private bool serverStarted;
        private int result = 1;
        private DateTime previousSample = DateTime.MinValue;
        private TimeSpan previousCpu = TimeSpan.Zero;

        private ServerGui(string exe, string[] args, string workingDirectory)
        {
            executable = exe;
            arguments = args;
            directory = workingDirectory;
            Text = "Minecraft server - IndustrialRust1710 (1.7.10)";
            StartPosition = FormStartPosition.CenterScreen;
            MinimumSize = new Size(730, 470);
            ClientSize = new Size(990, 605);
            Font = new Font("Segoe UI", 9F);
            BuildLayout();
            refreshTimer.Interval = 200;
            refreshTimer.Tick += delegate { OnRefresh(); };
            Shown += delegate { StartServer(); };
            FormClosing += OnClosing;
        }

        public static int Run(string exe, string[] args, string directory)
        {
            Application.EnableVisualStyles();
            Application.SetCompatibleTextRenderingDefault(false);
            using (ServerGui window = new ServerGui(exe, args, directory))
            {
                Application.Run(window);
                return window.result;
            }
        }

        private void BuildLayout()
        {
            SplitContainer split = new SplitContainer();
            split.Dock = DockStyle.Fill;
            // Establish bounds before assigning minimum panel sizes.
            split.Size = new Size(990, 570);
            split.Orientation = Orientation.Vertical;
            split.SplitterWidth = 6;
            split.Panel1MinSize = 245;
            split.Panel2MinSize = 350;
            split.SplitterDistance = 365;

            TableLayoutPanel left = new TableLayoutPanel();
            left.Dock = DockStyle.Fill;
            left.RowCount = 2;
            left.ColumnCount = 1;
            left.RowStyles.Add(new RowStyle(SizeType.Percent, 47));
            left.RowStyles.Add(new RowStyle(SizeType.Percent, 53));

            GroupBox statsGroup = new GroupBox();
            statsGroup.Text = "Stats";
            statsGroup.Dock = DockStyle.Fill;
            statsGroup.Padding = new Padding(8);
            TableLayoutPanel statsLayout = new TableLayoutPanel();
            statsLayout.Dock = DockStyle.Fill;
            statsLayout.RowCount = 2;
            statsLayout.RowStyles.Add(new RowStyle(SizeType.Percent, 53));
            statsLayout.RowStyles.Add(new RowStyle(SizeType.Percent, 47));
            graph.Dock = DockStyle.Fill;
            graph.BackColor = Color.White;
            graph.BorderStyle = BorderStyle.FixedSingle;
            graph.Paint += PaintGraph;
            stats.Dock = DockStyle.Fill;
            stats.Text = "Starting...\r\nMemory: waiting for server\r\nAvg tick: not available";
            statsLayout.Controls.Add(graph, 0, 0);
            statsLayout.Controls.Add(stats, 0, 1);
            statsGroup.Controls.Add(statsLayout);

            GroupBox playersGroup = new GroupBox();
            playersGroup.Text = "Players";
            playersGroup.Dock = DockStyle.Fill;
            players.Dock = DockStyle.Fill;
            players.BorderStyle = BorderStyle.None;
            playersGroup.Controls.Add(players);
            left.Controls.Add(statsGroup, 0, 0);
            left.Controls.Add(playersGroup, 0, 1);
            split.Panel1.Controls.Add(left);

            GroupBox consoleGroup = new GroupBox();
            consoleGroup.Text = "Log and chat";
            consoleGroup.Dock = DockStyle.Fill;
            consoleGroup.Padding = new Padding(8);
            TableLayoutPanel consoleLayout = new TableLayoutPanel();
            consoleLayout.Dock = DockStyle.Fill;
            consoleLayout.RowCount = 2;
            consoleLayout.RowStyles.Add(new RowStyle(SizeType.Percent, 100));
            consoleLayout.RowStyles.Add(new RowStyle(SizeType.Absolute, 36));

            log.Dock = DockStyle.Fill;
            log.ReadOnly = true;
            log.BackColor = Color.White;
            log.ForeColor = Color.Black;
            log.WordWrap = false;
            log.Font = new Font("Consolas", 9F);
            log.HideSelection = false;

            TableLayoutPanel input = new TableLayoutPanel();
            input.Dock = DockStyle.Fill;
            input.ColumnCount = 3;
            input.ColumnStyles.Add(new ColumnStyle(SizeType.Percent, 100));
            input.ColumnStyles.Add(new ColumnStyle(SizeType.Absolute, 70));
            input.ColumnStyles.Add(new ColumnStyle(SizeType.Absolute, 80));
            command.Dock = DockStyle.Fill;
            command.Enabled = false;
            command.KeyDown += delegate(object sender, KeyEventArgs e) {
                if (e.KeyCode == Keys.Enter) {
                    e.SuppressKeyPress = true;
                    SendCurrentCommand();
                }
            };
            send.Text = "Send";
            send.Dock = DockStyle.Fill;
            send.Enabled = false;
            send.Click += delegate { SendCurrentCommand(); };
            stop.Text = "Stop";
            stop.Dock = DockStyle.Fill;
            stop.Enabled = false;
            stop.Click += delegate { RequestStop(); };
            input.Controls.Add(command, 0, 0);
            input.Controls.Add(send, 1, 0);
            input.Controls.Add(stop, 2, 0);

            consoleLayout.Controls.Add(log, 0, 0);
            consoleLayout.Controls.Add(input, 0, 1);
            consoleGroup.Controls.Add(consoleLayout);
            split.Panel2.Controls.Add(consoleGroup);

            StatusStrip footer = new StatusStrip();
            status.Text = "Starting...";
            footer.Items.Add(status);
            Controls.Add(split);
            Controls.Add(footer);
        }

        // Compatible with Windows PowerShell 5.1/.NET Framework (no ArgumentList).
        private static string QuoteArgument(string value)
        {
            if (value.Length > 0 && value.IndexOfAny(new char[] {' ', '\t', '\n', '\v', '"'}) < 0)
                return value;
            StringBuilder quoted = new StringBuilder("\"");
            int backslashes = 0;
            foreach (char c in value) {
                if (c == '\\') {
                    backslashes++;
                } else if (c == '"') {
                    quoted.Append('\\', backslashes * 2 + 1);
                    quoted.Append('"');
                    backslashes = 0;
                } else {
                    quoted.Append('\\', backslashes);
                    quoted.Append(c);
                    backslashes = 0;
                }
            }
            quoted.Append('\\', backslashes * 2);
            quoted.Append('"');
            return quoted.ToString();
        }

        private static string QuoteArguments(string[] values)
        {
            List<string> result = new List<string>();
            foreach (string value in values)
                result.Add(QuoteArgument(value));
            return string.Join(" ", result.ToArray());
        }

        private void StartServer()
        {
            try {
                ProcessStartInfo info = new ProcessStartInfo(executable, QuoteArguments(arguments));
                info.WorkingDirectory = directory;
                info.UseShellExecute = false;
                info.CreateNoWindow = true;
                info.RedirectStandardInput = true;
                info.RedirectStandardOutput = true;
                info.RedirectStandardError = true;
                info.StandardOutputEncoding = Encoding.UTF8;
                info.StandardErrorEncoding = Encoding.UTF8;
                server = new Process();
                server.StartInfo = info;
                server.OutputDataReceived += delegate(object sender, DataReceivedEventArgs e) {
                    if (e.Data != null) Enqueue(e.Data);
                };
                server.ErrorDataReceived += delegate(object sender, DataReceivedEventArgs e) {
                    if (e.Data != null) Enqueue(e.Data);
                };
                server.Start();
                serverStarted = true;
                server.StandardInput.AutoFlush = true;
                server.BeginOutputReadLine();
                server.BeginErrorReadLine();
                command.Enabled = true;
                send.Enabled = true;
                stop.Enabled = true;
                status.Text = "Running (Minecraft Java 1.7.10)";
                refreshTimer.Start();
            } catch (Exception error) {
                AppendLog("ERROR: Could not start server: " + error.Message);
                status.Text = "Startup failed";
            }
        }

        // DataReceived callbacks run on background threads. Never touch WinForms here.
        private void Enqueue(string line)
        {
            lock (pending) {
                if (pending.Count >= 5000) {
                    pending.Dequeue();
                }
                pending.Enqueue(line);
            }
        }

        private void AppendLog(string line)
        {
            Match joined = Regex.Match(line, @"^joined: ([A-Za-z0-9_]{1,16}) \(");
            Match left = Regex.Match(line, @"^left: ([A-Za-z0-9_]{1,16})\s*$");
            if (joined.Success) {
                online.Add(joined.Groups[1].Value);
                UpdatePlayers();
            } else if (left.Success) {
                online.Remove(left.Groups[1].Value);
                UpdatePlayers();
            }
            // Bound the log memory usage even for noisy servers.
            if (log.TextLength > 220000) {
                log.Select(0, 70000);
                log.SelectedText = "";
            }
            log.AppendText(line + Environment.NewLine);
        }

        private void UpdatePlayers()
        {
            List<string> names = new List<string>(online);
            names.Sort(StringComparer.OrdinalIgnoreCase);
            players.BeginUpdate();
            players.Items.Clear();
            foreach (string name in names)
                players.Items.Add(name);
            players.EndUpdate();
        }

        private void OnRefresh()
        {
            int drained = 0;
            while (drained < 400) {
                string line;
                lock (pending) {
                    if (pending.Count == 0) break;
                    line = pending.Dequeue();
                }
                AppendLog(line);
                drained++;
            }
            if (server == null || !serverStarted) return;
            try {
                if (server.HasExited) {
                    if (!exitObserved) {
                        exitObserved = true;
                        result = server.ExitCode;
                        AppendLog("Server process exited (code " + result + ").");
                        status.Text = "Stopped (exit code " + result + ")";
                        command.Enabled = false;
                        send.Enabled = false;
                        stop.Enabled = false;
                        stats.Text = "Server stopped\r\nPlayers: 0\r\nAvg tick: not available";
                        online.Clear();
                        UpdatePlayers();
                    }
                    if (closing) {
                        refreshTimer.Stop();
                        BeginInvoke((MethodInvoker)delegate { Close(); });
                    }
                } else {
                    SampleStats();
                }
            } catch (InvalidOperationException) {
                // The process may have exited while metrics were being sampled.
            }
        }

        private void SampleStats()
        {
            if (previousSample != DateTime.MinValue &&
                (DateTime.UtcNow - previousSample).TotalMilliseconds < 1000)
                return;
            server.Refresh();
            DateTime now = DateTime.UtcNow;
            TimeSpan cpu = server.TotalProcessorTime;
            double cpuPercent = 0;
            if (previousSample != DateTime.MinValue) {
                double elapsed = (now - previousSample).TotalMilliseconds;
                if (elapsed > 0)
                    cpuPercent = Math.Max(0, (cpu - previousCpu).TotalMilliseconds /
                        elapsed / Environment.ProcessorCount * 100.0);
            }
            previousSample = now;
            previousCpu = cpu;
            long memory = server.WorkingSet64;
            workingSetHistory.Add(memory);
            if (workingSetHistory.Count > 160) workingSetHistory.RemoveAt(0);
            stats.Text = "Memory use: " + (memory / (1024 * 1024)) + " MiB (process)\r\n" +
                "Threads: " + server.Threads.Count + "\r\n" +
                "CPU: " + cpuPercent.ToString("0.0") + "% (all cores)\r\n" +
                "Avg tick: not available\r\n" +
                "Players: " + online.Count + "/8";
            graph.Invalidate();
        }

        private void PaintGraph(object sender, PaintEventArgs e)
        {
            Graphics g = e.Graphics;
            g.Clear(Color.White);
            int h = Math.Max(1, graph.ClientSize.Height - 7);
            int width = Math.Max(1, graph.ClientSize.Width - 2);
            long peak = 128L * 1024 * 1024;
            foreach (long sample in workingSetHistory)
                if (sample > peak) peak = sample;
            int count = Math.Min(120, workingSetHistory.Count);
            int left = workingSetHistory.Count - count;
            using (SolidBrush brush = new SolidBrush(Color.FromArgb(130, 0, 0))) {
                for (int i = 0; i < count; i++) {
                    int x0 = 1 + i * width / 120;
                    int x1 = 1 + (i + 1) * width / 120;
                    int bar = (int)(workingSetHistory[left + i] * (long)h / peak);
                    g.FillRectangle(brush, x0, h - bar + 1,
                        Math.Max(1, x1 - x0), Math.Max(1, bar));
                }
            }
            using (Pen pen = new Pen(Color.DarkRed)) {
                g.DrawLine(pen, 0, h + 1, width + 1, h + 1);
            }
        }

        private void SendCurrentCommand()
        {
            string line = command.Text.Trim();
            if (line.Length == 0) return;
            // Vanilla's GUI accepts "list". This Rust server currently expects "/list".
            if (!line.StartsWith("/", StringComparison.Ordinal)) line = "/" + line;
            if (SendCommand(line)) {
                AppendLog("> " + line);
                command.Clear();
            }
        }

        private bool SendCommand(string line)
        {
            if (server == null || !serverStarted || server.HasExited || stopping) return false;
            try {
                server.StandardInput.WriteLine(line);
                return true;
            } catch (Exception error) {
                AppendLog("ERROR: Cannot send command: " + error.Message);
                return false;
            }
        }

        private void RequestStop()
        {
            if (stopping || server == null || !serverStarted || server.HasExited) return;
            if (SendCommand("/stop")) {
                stopping = true;
                stop.Enabled = false;
                command.Enabled = false;
                send.Enabled = false;
                status.Text = "Stopping gracefully (saving world)...";
                AppendLog("> /stop");
            }
        }

        private void OnClosing(object sender, FormClosingEventArgs e)
        {
            if (server == null || !serverStarted || server.HasExited) return;
            e.Cancel = true;
            closing = true;
            RequestStop();
            if (stopping)
                status.Text = "Waiting for world save and player disconnects...";
        }

        protected override void Dispose(bool disposing)
        {
            if (disposing) {
                refreshTimer.Dispose();
                if (server != null) server.Dispose();
            }
            base.Dispose(disposing);
        }
    }
}
