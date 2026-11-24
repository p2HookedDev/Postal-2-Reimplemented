class ModernMenuSurfaces extends ModernMenuPage;

var localized string    ReflectionNames[4];

function CreateMenuContents()
{
	local UWindowComboControl Combo;
	local int i;

	Super.CreateMenuContents();

	AddTitle(TitleText, TitleFont, TitleAlign);

	bUpdate = false;
	AddBox(0, "ModernRelief");
	AddBox(1, "ModernSpecular");
	AddBox(2, "ModernTessellation");
	AddBox(3, "ModernTerrainDetail");
	Combo = AddLevelCombo(4, "ModernReflections", 2, 3);
	for (i = 0; i < ArrayCount(ReflectionNames); i++)
		Combo.AddItem(ReflectionNames[i]);
	AddLevelSlider(5, "ModernReflectionStrength", 0, 10, 10);
	AddBack();
	LoadRows();
}

function RowChanged(int i)
{
}

defaultproperties
{
	TitleText="Surfaces and Reflections"
	BackHelp="Return to the Modern Graphics menu."

	ReflectionNames(0)="Off"
	ReflectionNames(1)="Low"
	ReflectionNames(2)="Medium"
	ReflectionNames(3)="High"

	Labels(0)="Bump mapping"
	Labels(1)="Specular highlights"
	Labels(2)="Tessellation"
	Labels(3)="Terrain tessellation"
	Labels(4)="Screen space reflections"
	Labels(5)="Reflection strength"

	Helps(0)="Walls and floors catch the light with bumps taken from their textures."
	Helps(1)="Surfaces shine by their material: metal, tile, wood, cloth and skin differ."
	Helps(2)="Rounds the outlines of people and animals."
	Helps(3)="Hills and slopes look rounded instead of faceted; applies when a level loads."
	Helps(4)="Shiny surfaces reflect what is on screen."
	Helps(5)="How strongly shiny surfaces mirror their surroundings."
}
