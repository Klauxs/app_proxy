param([Parameter(Mandatory=$true)][ValidatePattern('^[a-f0-9]{12}$')][string]$Key,[switch]$StopTrace)
$ErrorActionPreference = 'Stop'
# Self-contained administrator-only payload; never load code from the writable app directory.
Add-Type -ReferencedAssemblies System.Web.Extensions -TypeDefinition @'
using System;
using System.ComponentModel;
using System.Diagnostics;
using System.IO;
using System.IO.Pipes;
using System.Runtime.InteropServices;
using System.Security.AccessControl;
using System.Security.Cryptography;
using System.Security.Principal;
using System.Text;
using System.Threading;
using System.Web.Script.Serialization;
public static class AppProxyElevatedEvents {
  static readonly object gate = new object();
  static readonly ManualResetEvent stopped = new ManualResetEvent(false);
  static readonly Guid provider = new Guid("22fb2cd6-0e7b-422b-a0c7-2fad1fd0e716");
  static StreamWriter writer;
  static volatile bool closing;
  static int session;
  const int FlushMs = 100;
  [StructLayout(LayoutKind.Sequential)]
  struct Wnode {
    public uint BufferSize, ProviderId;
    public ulong HistoricalContext, TimeStamp;
    public Guid Guid;
    public uint ClientContext, Flags;
  }
  [StructLayout(LayoutKind.Sequential, CharSet=CharSet.Unicode)]
  struct Properties {
    public Wnode Wnode;
    public uint BufferSize, MinimumBuffers, MaximumBuffers, MaximumFileSize, LogFileMode, FlushTimer, EnableFlags, AgeLimit;
    public uint NumberOfBuffers, FreeBuffers, EventsLost, BuffersWritten, LogBuffersLost, RealTimeBuffersLost;
    public IntPtr LoggerThreadId;
    public uint LogFileNameOffset, LoggerNameOffset;
    [MarshalAs(UnmanagedType.ByValTStr, SizeConst=1024)] public string LoggerName;
  }
  // Windows x64 EVENT_TRACE_LOGFILEW ABI, including EVENT_TRACE and TRACE_LOGFILE_HEADER.
  // This release is x64; reject other ABIs before calling native APIs.
  [StructLayout(LayoutKind.Explicit, Size=448)]
  struct LogFile {
    [FieldOffset(8)] public IntPtr LoggerName;
    [FieldOffset(28)] public uint ProcessTraceMode;
    [FieldOffset(424)] public IntPtr EventRecordCallback;
  }
  [StructLayout(LayoutKind.Explicit, Size=80)]
  struct Header {
    [FieldOffset(16)] public long TimeStamp;
    [FieldOffset(24)] public Guid ProviderId;
    [FieldOffset(40)] public ushort Id;
  }
  [StructLayout(LayoutKind.Sequential)]
  struct PropertyData { public ulong PropertyName; public uint ArrayIndex, Reserved; }
  [UnmanagedFunctionPointer(CallingConvention.Winapi)] delegate void RecordCallback(IntPtr record);
  [DllImport("advapi32.dll", CharSet=CharSet.Unicode)] static extern uint StartTraceW(out ulong handle, string name, ref Properties properties);
  [DllImport("advapi32.dll", CharSet=CharSet.Unicode)] static extern uint ControlTraceW(ulong handle, string name, ref Properties properties, uint control);
  [DllImport("advapi32.dll")] static extern uint EnableTraceEx2(ulong handle, ref Guid providerId, uint control, byte level, ulong anyKeyword, ulong allKeyword, uint timeout, IntPtr parameters);
  [DllImport("advapi32.dll", CharSet=CharSet.Unicode, SetLastError=true)] static extern ulong OpenTraceW(ref LogFile file);
  [DllImport("advapi32.dll")] static extern uint ProcessTrace([In] ulong[] handles, uint count, IntPtr start, IntPtr end);
  [DllImport("advapi32.dll")] static extern uint CloseTrace(ulong handle);
  [DllImport("tdh.dll")] static extern uint TdhGetPropertySize(IntPtr record, uint contextCount, IntPtr context, uint count, ref PropertyData property, out uint size);
  [DllImport("tdh.dll")] static extern uint TdhGetProperty(IntPtr record, uint contextCount, IntPtr context, uint count, ref PropertyData property, uint size, [Out] byte[] data);
  static Properties NewProperties(string name, Guid id) {
    var p = new Properties();
    p.Wnode.BufferSize = (uint)Marshal.SizeOf(typeof(Properties));
    p.Wnode.Guid = id; p.Wnode.ClientContext = 1; p.Wnode.Flags = 0x20000; // QPC, WNODE_FLAG_TRACED_GUID
    p.BufferSize = 16; p.MinimumBuffers = 4; p.MaximumBuffers = 16;
    p.LogFileMode = 0x100 | 0x10000000; // REAL_TIME | NO_PER_PROCESSOR_BUFFERING; no ETL file
    p.FlushTimer = 1;
    p.LoggerNameOffset = (uint)Marshal.OffsetOf(typeof(Properties), "LoggerName").ToInt32();
    p.LoggerName = name;
    return p;
  }
  static void Check(uint code) { if (code != 0) throw new Win32Exception((int)code); }
  static double Milliseconds(DateTime value) { return (value - new DateTime(1970,1,1)).TotalMilliseconds; }
  static void Send(object value) {
    lock (gate) {
      if (closing) return;
      try { writer.WriteLine(new JavaScriptSerializer().Serialize(value)); writer.Flush(); }
      catch (IOException) { closing = true; stopped.Set(); }
      catch (ObjectDisposedException) { closing = true; stopped.Set(); }
    }
  }
  static void Fail(string reason) { Send(new {type="error", reason=reason}); stopped.Set(); }
  static byte[] ReadProperty(IntPtr record, string name) {
    IntPtr text = Marshal.StringToHGlobalUni(name);
    try {
      var property = new PropertyData { PropertyName=(ulong)text.ToInt64(), ArrayIndex=uint.MaxValue };
      uint size;
      Check(TdhGetPropertySize(record, 0, IntPtr.Zero, 1, ref property, out size));
      if (size == 0 || size > 65536) throw new InvalidDataException("Invalid ETW property size");
      var data = new byte[size];
      Check(TdhGetProperty(record, 0, IntPtr.Zero, 1, ref property, size, data));
      return data;
    } finally { Marshal.FreeHGlobal(text); }
  }
  static void OnRecord(IntPtr record) {
    if (closing || stopped.WaitOne(0)) return;
    double callbackAt = Milliseconds(DateTime.UtcNow);
    try {
      var header = (Header)Marshal.PtrToStructure(record, typeof(Header));
      if (header.ProviderId != provider || header.Id != 1) return; // ProcessStart only, not rundown/stop/thread events
      // TDH resolves version-dependent payload fields by name.
      if (BitConverter.ToUInt32(ReadProperty(record, "SessionID"), 0) != session) return;
      uint pid = BitConverter.ToUInt32(ReadProperty(record, "ProcessID"), 0);
      string path = Encoding.Unicode.GetString(ReadProperty(record, "ImageName")).TrimEnd('\0');
      string name = Path.GetFileName(path);
      if (pid == 0 || pid > int.MaxValue || String.IsNullOrEmpty(name) || name.Length > 260) return;
      Send(new {type="start", pid=(int)pid, name=name, eventAt=Milliseconds(DateTime.FromFileTimeUtc(header.TimeStamp)), callbackAt=callbackAt});
    } catch (Exception e) { Fail("etw-decode-" + e.GetType().Name); }
  }
  static Guid SessionGuid(string name) {
    using (var hash = SHA256.Create()) { var bytes = hash.ComputeHash(Encoding.UTF8.GetBytes(name)); var guid = new byte[16]; Array.Copy(bytes, guid, 16); return new Guid(guid); }
  }
  static string SessionName(string key) { return "AppProxy.ProcessStart." + WindowsIdentity.GetCurrent().User.Value + "." + key; }
  public static void StopOwnedSession(string key) {
    if (!new WindowsPrincipal(WindowsIdentity.GetCurrent()).IsInRole(WindowsBuiltInRole.Administrator)) throw new UnauthorizedAccessException();
    string name = SessionName(key);
    Guid id = SessionGuid(name);
    var existing = NewProperties(name, id);
    uint result = ControlTraceW(0, name, ref existing, 0);
    if (result == 4201) return; // ERROR_WMI_INSTANCE_NOT_FOUND
    Check(result);
    if (existing.Wnode.Guid != id) throw new InvalidOperationException("ETW session name belongs to another owner");
    Check(ControlTraceW(0, name, ref existing, 1));
  }
  static void Trace(NamedPipeServerStream pipe, string name) {
    // Stable identity permits recovery after forced task termination, including a previous logon session.
    Guid id = SessionGuid(name);
    var properties = NewProperties(name, id);
    ulong handle;
    uint result = StartTraceW(out handle, name, ref properties);
    if (result == 183) {
      var existing = NewProperties(name, id);
      Check(ControlTraceW(0, name, ref existing, 0));
      if (existing.Wnode.Guid != id) throw new InvalidOperationException("ETW session name belongs to another owner");
      Check(ControlTraceW(0, name, ref existing, 1));
      properties = NewProperties(name, id);
      result = StartTraceW(out handle, name, ref properties);
    }
    Check(result);
    ulong consumer = ulong.MaxValue;
    Thread processing = null;
    IntPtr loggerName = Marshal.StringToHGlobalUni(name);
    RecordCallback callback = OnRecord;
    try {
      var file = new LogFile { LoggerName=loggerName, ProcessTraceMode=0x100 | 0x10000000,
        EventRecordCallback=Marshal.GetFunctionPointerForDelegate(callback) }; // REAL_TIME | EVENT_RECORD
      consumer = OpenTraceW(ref file);
      if (consumer == ulong.MaxValue) throw new Win32Exception(Marshal.GetLastWin32Error());
      var processProvider = provider;
      Check(EnableTraceEx2(handle, ref processProvider, 1, 4, 0x10, 0, 5000, IntPtr.Zero));
      processing = new Thread(delegate() {
        uint error = ProcessTrace(new ulong[] {consumer}, 1, IntPtr.Zero, IntPtr.Zero);
        if (!closing) Fail("etw-consumer-stopped-" + error);
      });
      processing.IsBackground = true; processing.Start();
      var reader = new Thread(delegate() {
        try { while (pipe.ReadByte() >= 0) {} } // EOF only: no privileged commands accepted.
        catch (IOException) {} catch (ObjectDisposedException) {}
        finally { stopped.Set(); }
      });
      reader.IsBackground = true; reader.Start();
      Send(new {type="ready", elevated=true, source="etw", flushMs=FlushMs, pid=Process.GetCurrentProcess().Id});
      while (!stopped.WaitOne(FlushMs)) {
        // Built-in ETW timer has a one-second floor. Explicit flush delivers sparse events sooner.
        Check(ControlTraceW(handle, null, ref properties, 3));
        if (properties.EventsLost != 0 || properties.RealTimeBuffersLost != 0) { Fail("etw-events-lost"); break; }
      }
    } catch (Exception e) { Fail("etw-" + e.GetType().Name + "-" + (e is Win32Exception ? ((Win32Exception)e).NativeErrorCode.ToString() : "failure")); }
    finally {
      closing = true;
      ControlTraceW(handle, null, ref properties, 1); // stop only the session we created
      if (consumer != ulong.MaxValue) CloseTrace(consumer);
      if (processing != null) processing.Join();
      GC.KeepAlive(callback);
      Marshal.FreeHGlobal(loggerName);
    }
  }
  public static void Run(string key) {
    if (IntPtr.Size != 8) throw new PlatformNotSupportedException("ETW listener requires x64 PowerShell");
    var identity = WindowsIdentity.GetCurrent();
    if (!new WindowsPrincipal(identity).IsInRole(WindowsBuiltInRole.Administrator)) throw new UnauthorizedAccessException();
    var sid = identity.User;
    session = Process.GetCurrentProcess().SessionId;
    var acl = new PipeSecurity();
    acl.SetAccessRuleProtection(true, false);
    acl.AddAccessRule(new PipeAccessRule(new SecurityIdentifier(WellKnownSidType.NetworkSid, null), PipeAccessRights.FullControl, AccessControlType.Deny));
    acl.AddAccessRule(new PipeAccessRule(sid, PipeAccessRights.ReadWrite, AccessControlType.Allow));
    acl.AddAccessRule(new PipeAccessRule(new SecurityIdentifier(WellKnownSidType.BuiltinAdministratorsSid, null), PipeAccessRights.FullControl, AccessControlType.Allow));
    acl.AddAccessRule(new PipeAccessRule(new SecurityIdentifier(WellKnownSidType.LocalSystemSid, null), PipeAccessRights.FullControl, AccessControlType.Allow));
    using (var pipe = new NamedPipeServerStream("AppProxy-Events-" + sid.Value + "-" + key + "-" + session, PipeDirection.InOut, 1, PipeTransmissionMode.Byte, PipeOptions.Asynchronous, 4096, 4096, acl)) {
      if (!pipe.WaitForConnectionAsync().Wait(30000)) return;
      using (writer = new StreamWriter(pipe, new UTF8Encoding(false), 4096, true)) {
        try { Trace(pipe, SessionName(key)); }
        catch (Exception e) { Fail("etw-" + e.GetType().Name); }
      }
    }
  }
}
'@
if ($StopTrace) { [AppProxyElevatedEvents]::StopOwnedSession($Key) } else { [AppProxyElevatedEvents]::Run($Key) }
