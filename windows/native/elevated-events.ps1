param([Parameter(Mandatory=$true)][ValidatePattern('^[a-f0-9]{12}$')][string]$Key)
$ErrorActionPreference = 'Stop'
# Installed under Program Files with administrator-only write access.
# The pipe publishes notifications only; client input is never executed or parsed.
Add-Type -ReferencedAssemblies System.Management,System.Web.Extensions -TypeDefinition @'
using System;
using System.Diagnostics;
using System.IO;
using System.IO.Pipes;
using System.Management;
using System.Security.AccessControl;
using System.Security.Principal;
using System.Text;
using System.Web.Script.Serialization;
public static class AppProxyElevatedEvents {
  static readonly object gate = new object();
  static StreamWriter writer;
  static bool closing;
  static void Send(object value) {
    lock (gate) {
      if (closing) return;
      try { writer.WriteLine(new JavaScriptSerializer().Serialize(value)); writer.Flush(); }
      catch (IOException) { closing = true; }
    }
  }
  public static void Run(string key) {
    var identity = WindowsIdentity.GetCurrent();
    if (!new WindowsPrincipal(identity).IsInRole(WindowsBuiltInRole.Administrator)) throw new UnauthorizedAccessException();
    var sid = identity.User;
    int session = Process.GetCurrentProcess().SessionId;
    var acl = new PipeSecurity();
    acl.SetAccessRuleProtection(true, false);
    acl.AddAccessRule(new PipeAccessRule(new SecurityIdentifier(WellKnownSidType.NetworkSid, null), PipeAccessRights.FullControl, AccessControlType.Deny));
    acl.AddAccessRule(new PipeAccessRule(sid, PipeAccessRights.ReadWrite, AccessControlType.Allow));
    acl.AddAccessRule(new PipeAccessRule(new SecurityIdentifier(WellKnownSidType.BuiltinAdministratorsSid, null), PipeAccessRights.FullControl, AccessControlType.Allow));
    acl.AddAccessRule(new PipeAccessRule(new SecurityIdentifier(WellKnownSidType.LocalSystemSid, null), PipeAccessRights.FullControl, AccessControlType.Allow));
    using (var pipe = new NamedPipeServerStream("AppProxy-Events-" + sid.Value + "-" + key + "-" + session, PipeDirection.InOut, 1, PipeTransmissionMode.Byte, PipeOptions.Asynchronous, 4096, 4096, acl)) {
      if (!pipe.WaitForConnectionAsync().Wait(30000)) return;
      using (writer = new StreamWriter(pipe, new UTF8Encoding(false), 4096, true))
      using (var watcher = new ManagementEventWatcher("SELECT * FROM Win32_ProcessStartTrace WHERE SessionID = " + session)) {
        bool started = false;
        watcher.EventArrived += delegate(object sender, EventArrivedEventArgs e) {
          double callbackAt = (DateTime.UtcNow - new DateTime(1970,1,1)).TotalMilliseconds;
          using (var item = e.NewEvent) Send(new {type="start", pid=Convert.ToInt32(item["ProcessID"]), name=Convert.ToString(item["ProcessName"]),
            eventAt=(DateTime.FromFileTimeUtc(Convert.ToInt64(item["TIME_CREATED"])) - new DateTime(1970,1,1)).TotalMilliseconds, callbackAt=callbackAt});
        };
        watcher.Stopped += delegate { if (started) Send(new {type="stopped"}); };
        try { watcher.Start(); }
        catch (ManagementException e) { Send(new {type="error", reason=e.ErrorCode.ToString()}); return; }
        started = true;
        Send(new {type="ready", elevated=true, pid=Process.GetCurrentProcess().Id});
        try { while (pipe.ReadByte() >= 0) {} } // EOF means Guard exited; there are no commands.
        catch (IOException) {}
        finally { lock (gate) { closing = true; } watcher.Stop(); }
      }
    }
  }
}
'@
[AppProxyElevatedEvents]::Run($Key)
