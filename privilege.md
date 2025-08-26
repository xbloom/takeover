
## 权限设置

a. 将程序的所有者改为 root：
```bash
sudo chown root test_proxy_manager
```
setuid 位只在程序执行时赋予程序其文件所有者的权限。所以，我们必须让 root 成为所有者。

b. 为程序添加 setuid 权限位：
```bash
sudo chmod u+s test_proxy_manager 
```
执行后，用 ls -l 查看，你会看到权限位变成了 -rwsr-xr-x，其中的 s 就代表 setuid

